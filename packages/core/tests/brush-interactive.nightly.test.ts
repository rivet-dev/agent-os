// Interactive-shell PTY regression test.
//
// brush's interactive line editor (reedline → crossterm) renders through the
// guest PTY. Reedline anchors its prompt at the row reported by
// `crossterm::cursor::position()`. The WASI crossterm port used to stub that to
// `(0, 0)`, so every repaint did `MoveTo(0,0)` + `Clear(FromCursorDown)` and
// wiped the whole screen — most visibly, pressing Enter erased the command and
// its output. The fix makes WASI `position()` issue a real DSR (`ESC[6n`) query
// and read the CPR reply, exactly like the Unix backend.
//
// This test drives the real brush shell over the sidecar shell API, renders the
// output with a headless xterm terminal emulator (which answers DSR like a real
// terminal), and snapshots the screen after each interactive step so a human can
// eyeball that scrollback survives, history recall works, and word-edit works.
//
// The guest command is staged under a UNIQUE name: a command literally named
// `sh` collides with the base-filesystem `/bin/sh` and the VM would run that
// default shell instead of the registry build under test.

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
import { Terminal } from "@xterm/headless";
import { afterEach, beforeAll, describe, expect, test } from "vitest";
import type { AgentOs } from "../src/index.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "../../..");
const SIDECAR_BINARY =
	process.env.AGENTOS_SIDECAR_BIN ??
	resolve(REPO_ROOT, "target/debug/agentos-sidecar");
const REGISTRY_SH_CANDIDATES = [
	"toolchain/target/wasm32-wasip1/release/commands/sh",
].map((candidate) => resolve(REPO_ROOT, candidate));
const REGISTRY_SH = REGISTRY_SH_CANDIDATES.find((candidate) =>
	existsSync(candidate),
);
const REGISTRY_CAT = REGISTRY_SH_CANDIDATES.map((candidate) =>
	resolve(dirname(candidate), "cat"),
).find(existsSync);
const FIXTURE_COMMAND = "brushsh"; // unique name so it does not shadow /bin/sh

let fixtureDir: string;

function snapshot(label: string, term: Terminal): string {
	const buffer = term.buffer.active;
	const lines: string[] = [];
	for (let row = 0; row < term.rows; row++) {
		const line = buffer.getLine(buffer.viewportY + row);
		lines.push(
			`${String(row + 1).padStart(2, "0")}|${line ? line.translateToString(true).replace(/\s+$/, "") : ""}`,
		);
	}
	return [
		`# ${label}`,
		`cursor=${buffer.cursorX},${buffer.cursorY}`,
		...lines,
	].join("\n");
}

async function waitFor(
	term: Terminal,
	text: string,
	timeoutMs = 20000,
): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		if (snapshot("w", term).includes(text)) {
			await new Promise((r) => setTimeout(r, 150));
			return;
		}
		await new Promise((r) => setTimeout(r, 25));
	}
	throw new Error(
		`timeout waiting for ${JSON.stringify(text)}\n${snapshot("timeout", term)}`,
	);
}

// Requires a freshly built registry `sh` command (`just registry-native-cmd sh`).
// Skip when the artifact is absent rather than testing a stale copied binary.
describe.skipIf(REGISTRY_SH === undefined)(
	"brush interactive PTY repaint",
	() => {
		let vm: AgentOs | undefined;
		let term: Terminal | undefined;
		let shellId: string | undefined;

		beforeAll(() => {
			// Materialize a self-contained `{ packageDir }` fixture: bin/<cmd> plus
			// the agentos-package.json manifest the sidecar projection requires.
			fixtureDir = mkdtempSync(join(tmpdir(), "brush-fixture-"));
			const binDir = join(fixtureDir, "bin");
			mkdirSync(binDir, { recursive: true });
			copyFileSync(REGISTRY_SH as string, join(binDir, FIXTURE_COMMAND));
			// A real external command (spawned as a CHILD of the shell) for the
			// child-output regression below; a unique name avoids /bin/cat.
			if (REGISTRY_CAT !== undefined) {
				copyFileSync(REGISTRY_CAT, join(binDir, "childcat"));
			}
			writeFileSync(
				join(fixtureDir, "package.json"),
				JSON.stringify({ name: "brush-fixture", version: "0.0.0" }),
			);
			writeFileSync(
				join(fixtureDir, "agentos-package.json"),
				JSON.stringify({ name: "brush-fixture", version: "1.0.0" }),
			);
			process.env.AGENTOS_SIDECAR_BIN = SIDECAR_BINARY;
		});

		afterEach(async () => {
			if (vm && shellId) {
				try {
					vm.closeShell(shellId);
				} catch {
					// already exited
				}
			}
			term?.dispose();
			if (vm) await vm.dispose();
			vm = term = shellId = undefined;
		});

		test("Enter preserves scrollback; history and word-edit work", async () => {
			const { AgentOs } = await import("../src/index.js");
			term = new Terminal({ cols: 80, rows: 14, allowProposedApi: true });
			vm = await AgentOs.create({
				software: [{ packagePath: fixtureDir }],
			});

			({ shellId } = vm.openShell({
				command: FIXTURE_COMMAND,
				args: ["--input-backend", "reedline", "-i"],
				cols: term.cols,
				rows: term.rows,
				env: {
					TERM: "xterm-256color",
					PS1: "AOS$ ",
					COLUMNS: "80",
					LINES: "14",
				},
			}));
			vm.onShellData(shellId, (d) => term?.write(d));
			const t = term;
			const s = shellId;
			const v = vm;
			// Forwarding xterm's responses back makes it answer DSR (`ESC[6n`) queries.
			t.onData((d) => v.writeShell(s, d));

			await waitFor(t, "AOS$");
			expect(snapshot("startup prompt", t)).toMatchSnapshot();

			// Run three commands. Each output must remain on screen after Enter.
			for (const word of ["alpha", "bravo", "charlie"]) {
				v.writeShell(s, `echo ${word}\r`);
				await waitFor(t, word);
			}
			expect(
				snapshot("after three commands (scrollback intact)", t),
			).toMatchSnapshot();

			// Up-arrow recalls the last command ("echo charlie").
			v.writeShell(s, "\x1b[A");
			await new Promise((r) => setTimeout(r, 300));
			expect(snapshot("after up-arrow recall", t)).toMatchSnapshot();

			// Ctrl-W deletes the recalled word ("charlie"), then type a new one and run it.
			v.writeShell(s, "\x17delta\r");
			// Wait for the new command's output line, then settle.
			await waitFor(t, "echo delta");
			await new Promise((r) => setTimeout(r, 400));
			expect(snapshot("after ctrl-w edit + enter", t)).toMatchSnapshot();
		}, 60000);

		test("restores cooked terminal state after a raw child exits", async () => {
			const { AgentOs } = await import("../src/index.js");
			term = new Terminal({ cols: 80, rows: 14, allowProposedApi: true });
			vm = await AgentOs.create({
				software: [{ packagePath: fixtureDir }],
			});
			await vm.writeFile(
				"/tmp/raw-child.mjs",
				"process.stdin.setRawMode(true); process.stdout.write('raw-child-exited\\n');\n",
			);
			await vm.writeFile(
				"/tmp/redirected-stdin-parent.mjs",
				[
					'import { spawnSync } from "node:child_process";',
					"const ignored = spawnSync('node', ['-e', `process.stdout.write('ignored-stdin-child\\\\n')`], { stdio: ['ignore', 'inherit', 'inherit'] });",
					"process.stdout.write('ignored-stdin-status:' + ignored.status + ' error:' + (ignored.error?.code ?? 'none') + '\\n');",
					"const piped = spawnSync('node', ['-e', `process.stdout.write('piped-stdin-child\\\\n')`], { input: '', stdio: ['pipe', 'inherit', 'inherit'] });",
					"process.stdout.write('piped-stdin-status:' + piped.status + ' error:' + (piped.error?.code ?? 'none') + '\\n');",
					"process.exit(ignored.status || piped.status || 0);",
				].join("\n"),
			);
			await vm.writeFile(
				"/tmp/cooked-check.mjs",
				"process.stdout.write('cooked-output-after-raw-child\\n');\n",
			);

			({ shellId } = vm.openShell({
				command: FIXTURE_COMMAND,
				args: ["--input-backend", "minimal", "-i"],
				cols: term.cols,
				rows: term.rows,
				env: {
					TERM: "xterm-256color",
					PS1: "AOS$ ",
				},
			}));
			vm.onShellData(shellId, (d) => term?.write(d));
			const t = term;
			const s = shellId;
			const v = vm;

			await waitFor(t, "AOS$");
			const promptsBeforeRedirectedStdin =
				snapshot("before redirected stdin", t).split("AOS$").length - 1;
			v.writeShell(s, "node /tmp/redirected-stdin-parent.mjs\r");
			await waitFor(t, "ignored-stdin-status:0 error:none");
			await waitFor(t, "piped-stdin-status:0 error:none");
			const redirectedPromptDeadline = Date.now() + 20_000;
			while (
				Date.now() < redirectedPromptDeadline &&
				snapshot("wait redirected", t).split("AOS$").length - 1 <=
					promptsBeforeRedirectedStdin
			) {
				await new Promise((resolve) => setTimeout(resolve, 25));
			}

			const promptsBeforeRaw =
				snapshot("before raw child", t).split("AOS$").length - 1;
			expect(promptsBeforeRaw).toBeGreaterThan(promptsBeforeRedirectedStdin);
			v.writeShell(s, "node /tmp/raw-child.mjs\r");
			await waitFor(t, "raw-child-exited");

			const promptDeadline = Date.now() + 20_000;
			while (
				Date.now() < promptDeadline &&
				snapshot("wait raw", t).split("AOS$").length - 1 <= promptsBeforeRaw
			) {
				await new Promise((resolve) => setTimeout(resolve, 25));
			}
			const afterRawChild = snapshot("after raw child", t);
			expect(afterRawChild.split("AOS$").length - 1).toBeGreaterThan(
				promptsBeforeRaw,
			);
			expect(afterRawChild).not.toContain(
				"could not retrieve pid for child process",
			);

			// A raw child disables ICRNL. If the sidecar does not restore the
			// parent's cooked termios, this carriage return never submits the line.
			v.writeShell(s, "node /tmp/cooked-check.mjs\r");
			await waitFor(t, "cooked-output-after-raw-child");
		}, 60000);

		// Regression: output from an EXTERNAL command (a child process sharing the
		// shell's terminal) must reach the host exactly once. It used to arrive
		// twice — once relayed by the shell's runner from the child's stdout
		// events, and once via the PTY master drain of the same bytes — doubling
		// every child's output (`cat` lines printed twice, vim keystroke echo
		// corrupting the screen).
		test.skipIf(REGISTRY_CAT === undefined)(
			"external child command output renders exactly once",
			async () => {
				const { AgentOs } = await import("../src/index.js");
				term = new Terminal({ cols: 80, rows: 14, allowProposedApi: true });
				vm = await AgentOs.create({
					software: [{ packagePath: fixtureDir }],
				});
				await vm.writeFile("/tmp/marker.txt", "child-once-marker\n");

				({ shellId } = vm.openShell({
					command: FIXTURE_COMMAND,
					args: ["--input-backend", "reedline", "-i"],
					cols: term.cols,
					rows: term.rows,
					env: {
						TERM: "xterm-256color",
						PS1: "AOS$ ",
						COLUMNS: "80",
						LINES: "14",
					},
				}));
				vm.onShellData(shellId, (d) => term?.write(d));
				const t = term;
				const s = shellId;
				const v = vm;
				t.onData((d) => v.writeShell(s, d));

				await waitFor(t, "AOS$");
				v.writeShell(s, "childcat /tmp/marker.txt\r");
				await waitFor(t, "child-once-marker");
				await new Promise((r) => setTimeout(r, 500));

				const rendered = snapshot("child output", t);
				const occurrences = rendered.split("child-once-marker").length - 1;
				expect(occurrences).toBe(1);
				expect(rendered).not.toContain(
					"could not retrieve pid for child process",
				);
				expect(rendered.lastIndexOf("child-once-marker")).toBeLessThan(
					rendered.lastIndexOf("AOS$"),
				);
			},
			60000,
		);
	},
);
