// Nightly: requires the non-core Vim command package.
import {
	copyFileSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import xterm from "@xterm/headless";
import { afterEach, describe, expect, it } from "vitest";
import type { AgentOs } from "../src/index.js";

const { Terminal } = xterm;

// The vim binary comes from the @agentos-software/vim registry package (built
// from source in agentos). Gate the suite on it being present so it skips
// on a checkout that has not built the registry.
const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const VIM_PACKAGE_BIN = resolve(
	REPO_ROOT,
	"../agentos/software/vim/dist/package/bin/vim",
);
const VIM_BINARY = process.env.AGENTOS_VIM_FIXTURE_BIN ?? VIM_PACKAGE_BIN;

const COLS = 80;
const ROWS = 24;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Render the terminal buffer to an array of trimmed rows (length ROWS). */
function rows(term: InstanceType<typeof Terminal>): string[] {
	const buf = term.buffer.active;
	const out: string[] = [];
	for (let y = 0; y < ROWS; y++) {
		const line = buf.getLine(y);
		out.push((line ? line.translateToString(true) : "").replace(/\s+$/, ""));
	}
	return out;
}

/**
 * Full-screen vim rendering over a real VM PTY. This is a STRICT layout guard:
 * it asserts exact screen geometry that a correct vim produces and that the
 * known-broken render violates — the status/ruler on the BOTTOM row (not
 * mashed onto a tilde a few rows up), the file message NOT stranded at the top,
 * the `-- INSERT --` indicator on entering insert mode, and the file written
 * byte-exact. Fuzzy `toContain` checks let a 2-row-offset render slip through;
 * these positional assertions do not.
 */
describe.skipIf(!existsSync(VIM_BINARY))("vim full-screen rendering (strict)", () => {
	let vm: AgentOs | undefined;
	afterEach(async () => {
		await vm?.dispose();
		vm = undefined;
	});

	it("lays out the screen with the status line on the bottom row", async () => {
		const { AgentOs } = await import("../src/index.js");
		const common = (await import("@agentos-software/common")).default;
		// Use the registry vim package by default; when AGENTOS_VIM_FIXTURE_BIN is
		// set, materialize a package around that binary (lets the same strict
		// assertions run against any candidate vim build).
		let vimPkg: unknown;
		if (process.env.AGENTOS_VIM_FIXTURE_BIN) {
			const dir = mkdtempSync(join(tmpdir(), "vim-render-"));
			mkdirSync(join(dir, "bin"));
			copyFileSync(process.env.AGENTOS_VIM_FIXTURE_BIN, join(dir, "bin", "vim"));
			writeFileSync(
				join(dir, "package.json"),
				JSON.stringify({ name: "vim", version: "0.0.0", bin: { vim: "bin/vim" } }),
			);
			writeFileSync(
				join(dir, "agentos-package.json"),
				JSON.stringify({ name: "vim", version: "1.0.0" }),
			);
			vimPkg = { packagePath: dir };
		} else {
			vimPkg = (await import("@agentos-software/vim")).default;
		}
		vm = await AgentOs.create({ software: [common, vimPkg] });
		await vm.mkdir("/work", { recursive: true });

		const term = new Terminal({ cols: COLS, rows: ROWS, allowProposedApi: true });
		let writes = Promise.resolve();
		// vim as the PTY's top-level process. This faithfully reproduces the
		// screen layout a real terminal (tmux) shows for `just shell` → `vim`:
		// full-screen renders, but the status line lands on the wrong row.
		const { shellId } = vm.openShell({
			command: "vim",
			args: ["-N", "-u", "NONE", "-i", "NONE", "-n", "/work/render.txt"],
			cols: COLS,
			rows: ROWS,
			cwd: "/work",
			env: { TERM: "xterm" },
		});
		vm.onShellData(shellId, (data) => {
			const bytes = Buffer.from(data);
			writes = writes.then(() => new Promise<void>((r) => term.write(bytes, r)));
		});
		const settle = async (ms = 700) => {
			await sleep(ms);
			await writes;
			await sleep(30);
			await writes;
		};
		// Wait until vim has actually painted the full-screen UI (a column of
		// tildes) before asserting layout — otherwise we would assert against the
		// transient startup/warning state. A timeout here means vim never entered
		// full-screen mode (e.g. it printed "not a terminal" and gave up).
		const waitForRender = async (timeoutMs = 20_000) => {
			const deadline = Date.now() + timeoutMs;
			while (Date.now() < deadline) {
				await settle(400);
				const r = rows(term);
				// "rendered" = vim painted its tilde column and cleared the startup
				// warnings. We intentionally accept the (broken) render here so the
				// precise layout assertions below are what fail, naming the defect.
				const tildes = r.filter((line) => line.startsWith("~")).length;
				if (tildes >= 10 && !r.some((l) => l.includes("not to a terminal")))
					return r;
			}
			throw new Error(
				`vim never rendered full-screen (still showing warnings):\n${rows(term).map((l, i) => `${i}|${l}`).join("\n")}`,
			);
		};

		const opened = await waitForRender();

		// (1) The empty buffer fills the window with `~` on every line EXCEPT the
		//     first content line (row 0) and the last (status) line (row 23).
		for (let y = 1; y <= ROWS - 2; y++) {
			expect(opened[y], `row ${y} should be a lone tilde`).toBe("~");
		}

		// (2) The status/ruler MUST be on the bottom row. The known-broken render
		//     leaves rows 22-23 blank and mashes the ruler onto a tilde ~2 rows up.
		expect(opened[ROWS - 1], "bottom row should hold the ruler").toMatch(
			/\d+,\d+(-\d+)?\s+All\s*$/,
		);
		// It must NOT sit on the same row as a tilde.
		expect(opened[ROWS - 1]).not.toMatch(/^~/);

		// (3) The "[New]" file message belongs on the command line (bottom area),
		//     NOT stranded at the very top mashed against the first tilde.
		expect(opened[0], "row 0 must not carry the file message + tilde").not.toContain(
			"render.txt",
		);
		expect(opened[0], "row 0 (empty buffer) starts blank").toBe("");

		// (4) Insert mode renders the typed text on the FIRST content row (vim
		//     draws it at the cursor via cursor addressing). The known-broken
		//     render instead ECHOES the keystrokes onto the bottom/status row and
		//     never places them on row 0 — so this row-0 assertion is the strict
		//     discriminator between a correct redraw and raw-echo garbling.
		await vm.writeShell(shellId, "i");
		await settle(900);
		await vm.writeShell(shellId, "The quick brown fox");
		await settle(900);
		const inserting = rows(term);
		expect(inserting[0], "typed text lands on the first content row").toContain(
			"The quick brown fox",
		);
		// The known-broken render echoes keystrokes onto the status row; the text
		// must NOT appear on the bottom row.
		expect(inserting[ROWS - 1], "text must not be echoed onto the status row").not.toContain(
			"quick brown fox",
		);
		// The status/ruler must still be on the bottom row (not scrolled away).
		expect(inserting[ROWS - 1], "bottom row still holds the ruler").toMatch(
			/\d+,\d+(-\d+)?/,
		);

		// (5) Write + quit; the file is byte-exact.
		await vm.writeShell(shellId, ":wq\r");
		await settle(1200);
		const content = Buffer.from(await vm.readFile("/work/render.txt")).toString(
			"utf8",
		);
		expect(content).toBe("The quick brown fox\n");
	}, 90_000);
});
