import { expect, test } from "@playwright/test";

// R3 probe: run the real built brush shell wasm in the browser worker, with
// WASI stdio bound to the kernel PTY slave and the host driving the PTY master.

test("real brush shell wasm spawns real external commands on browser PTY", async ({
	page,
}) => {
	await page.goto("/browser-real-shell.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const result = await page.evaluate(() => window.__browserRealShell!.run());

	if (result.error) throw new Error(result.error);
	expect(result.shellFetched).toBe(true);
	expect(result.masterFd).toBeGreaterThan(0);
	expect(result.slaveFd).toBeGreaterThan(0);
	expect(result.output).toContain("sh-0.4$ ");
	expect(result.output).toContain("browser-brush-ok");
	expect(result.output).toContain("browser-pipe-ok | /bin/wc -c");
	expect(result.output).toContain("16");
	expect(result.output).toContain("browser-cat-ok-via-cat");
	expect(result.output).toContain("browser-file-ok");
	expect(result.output).toContain("/bin/ls /");
	expect(result.output).toContain("etc");
	expect(result.output).toContain("^C");
	expect(result.output).toContain("browser-after-ctrl-c");
	expect(result.started).toBe(true);
});

declare global {
	interface Window {
		__browserRealShell?: {
			run(): Promise<{
				exitCode: number;
				masterFd?: number;
				slaveFd?: number;
				shellFetched: boolean;
				started: boolean;
				output: string;
				error?: string;
			}>;
		};
	}
}
