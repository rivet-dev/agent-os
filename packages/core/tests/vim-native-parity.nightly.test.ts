// Nightly: requires the non-core Vim command package.
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import type { AgentOs } from "../src/index.js";
import { createTerm, diffGrids } from "./helpers/term-diff.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "../../..");
const VIM_PACKAGE_BIN = resolve(REPO_ROOT, "software/vim/dist/package/bin/vim");
const NATIVE_VIM = "/usr/bin/vim";
const REF_SCRIPT = join(HERE, "helpers", "native-vim-ref.py");

const COLS = 80;
const ROWS = 24;
const PTY_OWNER_READY_TIMEOUT_MS = 90_000;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// Shared, ordered key sequence driven identically against native + wasm vim.
// `wait` is the settle time (seconds native / ms wasm) after the keys.
const STEPS = [
	{ label: "insert", keys: "i", wait: 0.5 },
	{ label: "type", keys: "The quick brown fox", wait: 0.6 },
	{ label: "newline", keys: "\rjumps over", wait: 0.6 },
	{ label: "esc", keys: "\x1b", wait: 0.4 },
	{ label: "gg", keys: "gg", wait: 0.4 },
	{ label: "cmdline", keys: ":set number\r", wait: 0.5 },
	{ label: "write", keys: ":w\r", wait: 0.6 },
];

interface Snap {
	label: string;
	raw: Uint8Array;
}

// Force identical, version-independent settings on both vims so a native-9.0
// vs wasm-9.2 default difference (e.g. `ruler`) can't masquerade as a behavior
// difference. With `set ruler` on both, the cursor position on the status line
// is itself part of the 1:1 assertion.
const VIM_ARGS = ["-c", "set ruler noshowcmd"];
const BASENAME = "parity.txt";

function nativeSnaps(file: string): Snap[] {
	const spec = {
		vim: NATIVE_VIM,
		cols: COLS,
		rows: ROWS,
		file,
		vimArgs: VIM_ARGS,
		openWait: 1.4,
		steps: STEPS.map((s) => ({
			label: s.label,
			keys_b64: Buffer.from(s.keys, "utf8").toString("base64"),
			wait: s.wait,
		})),
	};
	const out = execFileSync("python3", [REF_SCRIPT, JSON.stringify(spec)], {
		encoding: "utf8",
		maxBuffer: 32 * 1024 * 1024,
	});
	const parsed = JSON.parse(out) as {
		snaps: { label: string; raw_b64: string }[];
	};
	return parsed.snaps.map((s) => ({
		label: s.label,
		raw: new Uint8Array(Buffer.from(s.raw_b64, "base64")),
	}));
}

const canRun = existsSync(VIM_PACKAGE_BIN) && existsSync(NATIVE_VIM);

describe.skipIf(!canRun)("vim wasm vs native — 1:1 PTY parity", () => {
	let vm: AgentOs | undefined;
	afterEach(async () => {
		await vm?.dispose();
		vm = undefined;
	});

	it("renders identically to native vim through the same key sequence", async () => {
		const { AgentOs } = await import("../src/index.js");
		const common = (await import("@agentos-software/common")).default;
		const vimPkg = (await import("@agentos-software/vim")).default;

		// Reference: native vim over a real PTY.
		const native = nativeSnaps(`/tmp/${BASENAME}`);

		// Under test: wasm vim as a CHILD of the interactive brush shell (the
		// `just shell` path), so this exercises PTY-slave inheritance too. Wait for
		// each terminal-owner transition explicitly: shell prompt first, then Vim's
		// initial render, and only then send scripted Vim keys.
		vm = await AgentOs.create({ software: [common, vimPkg] });
		await vm.mkdir("/work", { recursive: true });
		const { shellId } = vm.openShell({
			command: "sh",
			args: ["--input-backend", "minimal", "-i"],
			cols: COLS,
			rows: ROWS,
			cwd: "/work",
			env: { TERM: "xterm", LANG: "C.UTF-8", PS1: "AOS$ " },
		});
		let cumulative = Buffer.alloc(0);
		const readinessTerm = createTerm(COLS, ROWS);
		let writes = Promise.resolve();
		vm.onShellData(shellId, (event) => {
			const b = Buffer.from(event.data);
			cumulative = Buffer.concat([cumulative, b]);
			writes = writes.then(() => readinessTerm.write(new Uint8Array(b)));
		});
		const settle = async (ms: number) => {
			await sleep(ms);
			await writes;
		};
		const waitForScreen = async (
			label: string,
			ready: (rows: string[]) => boolean,
		) => {
			const deadline = Date.now() + PTY_OWNER_READY_TIMEOUT_MS;
			while (Date.now() < deadline) {
				await settle(25);
				const rows = readinessTerm.grid().rows;
				if (ready(rows)) {
					await settle(250);
					return;
				}
			}
			throw new Error(
				`timed out waiting for ${label}:\n${readinessTerm
					.grid()
					.rows.map((row, index) => `${index}: ${JSON.stringify(row)}`)
					.join(
						"\n",
					)}\nprocesses:\n${JSON.stringify(vm?.allProcesses(), null, 2)}`,
			);
		};
		// The initial prompt may have been emitted before onShellData attached.
		// An empty line forces a fresh prompt and makes the handshake observable.
		await vm.writeShell(shellId, "\r");
		await waitForScreen("shell prompt", (rows) =>
			rows.some((row) => row.includes("AOS$")),
		);
		await vm.writeShell(
			shellId,
			`vim -N -u NONE -i NONE -n -c 'set ruler noshowcmd' /work/${BASENAME}\r`,
		);
		await waitForScreen(
			"vim readiness",
			(rows) =>
				rows.some((row) => row.includes(BASENAME)) &&
				rows.some((row) => row === "~"),
		);
		const wasmSnaps: Snap[] = [];
		const capture = (label: string) => {
			wasmSnaps.push({ label, raw: new Uint8Array(cumulative) });
		};
		capture("open");
		for (const step of STEPS) {
			await vm.writeShell(shellId, step.keys);
			await settle(step.wait * 1000);
			capture(step.label);
		}

		// Drive each side's stream through its own PERSISTENT emulator as
		// incremental deltas (bytes since the previous snapshot), then compare the
		// live grids step by step. Both snapshot lists are cumulative, so the delta
		// is the tail past the previous cumulative length. This mirrors how a real
		// terminal consumes bytes exactly once — full-replaying the cumulative
		// buffer into a fresh emulator would double-draw vim's scrolled redraws.
		const nativeTerm = createTerm(COLS, ROWS);
		const wasmTerm = createTerm(COLS, ROWS);
		let prevN = 0;
		let prevW = 0;
		const report: string[] = [];
		let allEqual = true;
		for (let i = 0; i < native.length; i++) {
			await nativeTerm.write(native[i].raw.subarray(prevN));
			await wasmTerm.write(wasmSnaps[i].raw.subarray(prevW));
			prevN = native[i].raw.length;
			prevW = wasmSnaps[i].raw.length;
			const d = diffGrids(native[i].label, nativeTerm.grid(), wasmTerm.grid(), {
				normalizeBasename: BASENAME,
			});
			report.push(...d.lines);
			if (!d.equal) allEqual = false;
		}
		if (!allEqual) {
			throw new Error(`vim wasm/native parity mismatch:\n${report.join("\n")}`);
		}
		expect(allEqual).toBe(true);
	}, 240_000);
});
