// pty-line-discipline.nightly.test.ts — terminal (PTY) line-discipline matrix.
//
// ONE shared CASES table is exercised IDENTICALLY through TWO guest runtimes:
//   - wasm-c : a C probe (tests/fixtures/pty/pty_probe.c) compiled to WASM and
//              registered as a VM command.
//   - js-node: a guest-Node probe (tests/fixtures/pty/pty_probe.mjs) launched via
//              the built-in `node` command.
// Both probes implement the same argv-dispatched case set and emit the same
// `#`-prefixed marker protocol, so the host asserts the SAME strings for both.
// The JS-node runtime is part of the default suite. The WASI-C runtime is gated
// behind AGENTOS_CORE_PTY_C=1 because it requires local C/WASI fixtures and is
// used as explicit native TTY validation.
//
// Each case's run(ctx) asserts the CORRECT (kernel-conformant) behavior. A cell
// is run under `test.fails` when it is effectively known-broken:
//     effectiveKnownBroken = case.knownBroken || (runtime === "js-node" && case.ttyDependent)
// so the correct-behavior assertion still RUNS and still throws honestly today —
// `test.fails` records that expected failure and will turn RED the moment the
// kernel / V8 TTY bridge is fixed (alerting that the flag is stale). Nothing is
// skipped for enabled runtimes.
//
// IMPORTANT (regression-guard integrity): for an it.fails cell the ONLY thing that
// may throw is the load-bearing behavior assertion — `ctx.snapshot()` does NOT
// assert for broken cells (see the snapshot() impl). If it did, a future FIX would
// change the screen, the stored snapshot would mismatch and throw, and it.fails
// would stay green — silently masking the fix. Snapshots are therefore recorded and
// asserted only for the PASSING (real `it`) cells; a regression there turns the cell
// RED, and a fix to a broken cell turns its it.fails RED.
//
// Signal-death cells (sigint/sigquit/vintr-buffer) POSITIVELY assert death: they
// `await waitShell` and require it to RESOLVE (status is never the 15s "timeout"),
// proving the signal actually terminated the process rather than the read merely
// hanging — the wasm PTY-signal kill surfaces a racy/zero exit code, so death is
// proven by "terminated + never reached #DONE", not by a 128+sig exit code.
//
// Run:
//   cd <repo root>
//   PATH="/home/nathan/.nvm/versions/node/v24.13.0/bin:/tmp/pnpm:/usr/bin:/bin" \
//     pnpm --dir packages/core exec vitest run tests/pty-line-discipline.nightly.test.ts

import { spawnSync } from "node:child_process";
import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Terminal } from "@xterm/headless";
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import type { AgentOs } from "../src/index.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "../../..");
const FIXTURE_DIR = resolve(__dirname, "fixtures/pty");
const C_PROBE_SOURCE = join(FIXTURE_DIR, "pty_probe.c");
const NODE_PROBE_SOURCE = join(FIXTURE_DIR, "pty_probe.mjs");
const NODE_PROBE_GUEST_PATH = "/pty_probe.mjs";

const WASI_SDK = resolve(
	REPO_ROOT,
	"toolchain/c/vendor/wasi-sdk",
);
const SIDECAR_BINARY = resolve(
	REPO_ROOT,
	process.env.CARGO_TARGET_DIR ?? "target",
	"debug/agentos-sidecar",
);

const SETTLE_MS = 80;
const WAIT_TIMEOUT_MS = 15_000;
const TEST_TIMEOUT_MS = 60_000;
const ENABLE_WASM_C_PTY = process.env.AGENTOS_CORE_PTY_C === "1";

// ---------------------------------------------------------------------------
// build / sidecar prerequisites
// ---------------------------------------------------------------------------

// Compile the C probe to WASM directly into a package `bin/` dir. The basename
// MUST be `pty_probe` (no extension): the sidecar registers the command from the
// package `bin/<name>` layout and `openShell({ command: "pty_probe" })` resolves
// it.
function buildCProbe(binDir: string): void {
	const clang = join(WASI_SDK, "bin/clang");
	if (!existsSync(clang)) {
		throw new Error(`wasi-sdk clang not found at ${clang}`);
	}
	const out = join(binDir, "pty_probe"); // basename MUST be `pty_probe`, no ext
	const result = spawnSync(
		clang,
		[
			"--target=wasm32-wasip1",
			`--sysroot=${join(WASI_SDK, "share/wasi-sysroot")}`,
			"-O2",
			"-o",
			out,
			C_PROBE_SOURCE,
		],
		{ encoding: "utf8" },
	);
	if (result.status !== 0) {
		throw new Error(
			["failed to build pty_probe.c", result.stdout, result.stderr]
				.filter(Boolean)
				.join("\n"),
		);
	}
	// Validate wasm magic so a bad build fails loudly here, not in the resolver.
	const magic = readFileSync(out).subarray(0, 4);
	if (!(magic[0] === 0x00 && magic[1] === 0x61 && magic[2] === 0x73 && magic[3] === 0x6d)) {
		throw new Error(`pty_probe build is not a wasm module (magic=${magic.toString("hex")})`);
	}
}

// Materialize a self-contained agentOS package directory holding the freshly
// built `pty_probe` WASM (`bin/pty_probe` + `package.json` + `agentos-package.json`).
// The sidecar projects `dir` into `/opt/agentos/bin/pty_probe` and registers it as
// a guest command. Returns the package dir to pass as `software: [<dir>]`.
function materializePtyProbePackage(): string {
	const pkgDir = mkdtempSync(join(tmpdir(), "pty-probe-pkg-"));
	const binDir = join(pkgDir, "bin");
	mkdirSync(binDir);
	buildCProbe(binDir);
	writeFileSync(
		join(pkgDir, "package.json"),
		JSON.stringify({ name: "pty-line-discipline-fixture", version: "0.0.0" }),
	);
	writeFileSync(
		join(pkgDir, "agentos-package.json"),
		JSON.stringify({ name: "pty-line-discipline-fixture", version: "1.0.0" }),
	);
	return pkgDir;
}

function ensureSidecarBuilt(): void {
	const configuredSidecar = process.env.AGENTOS_SIDECAR_BIN;
	if (configuredSidecar) {
		if (!existsSync(configuredSidecar)) {
			throw new Error(
				`AGENTOS_SIDECAR_BIN is set to ${configuredSidecar} but the file does not exist`,
			);
		}
		return;
	}
	if (!existsSync(SIDECAR_BINARY)) {
		const build = spawnSync("cargo", ["build", "-q", "-p", "agentos-sidecar"], {
			cwd: REPO_ROOT,
			encoding: "utf8",
		});
		if (build.status !== 0) {
			throw new Error(
				["failed to build agentos-sidecar", build.stdout, build.stderr]
					.filter(Boolean)
					.join("\n"),
			);
		}
	}
	process.env.AGENTOS_SIDECAR_BIN = SIDECAR_BINARY;
}

// ---------------------------------------------------------------------------
// terminal helpers
// ---------------------------------------------------------------------------

function snapshotLines(term: Terminal): string[] {
	const buffer = term.buffer.active;
	const lines: string[] = [];
	for (let row = 0; row < term.rows; row++) {
		const line = buffer.getLine(buffer.viewportY + row);
		lines.push(line ? line.translateToString(true) : "");
	}
	return lines;
}

function terminalSnapshot(label: string, term: Terminal): string {
	const buffer = term.buffer.active;
	const lines = snapshotLines(term).map(
		(line, i) => `${String(i + 1).padStart(2, "0")}|${line}`,
	);
	return [
		`# ${label}`,
		`cols=${term.cols} rows=${term.rows} cursor=${buffer.cursorX},${buffer.cursorY}`,
		...lines,
	].join("\n");
}

function screenText(term: Terminal): string {
	return snapshotLines(term).join("\n");
}

async function settle(): Promise<void> {
	await new Promise((r) => setTimeout(r, SETTLE_MS));
}

// ---------------------------------------------------------------------------
// case context
// ---------------------------------------------------------------------------

interface Runtime {
	name: "wasm-c" | "js-node";
}

interface Ctx {
	runtime: Runtime;
	broken: boolean;
	vm: AgentOs;
	shellId: string;
	term: Terminal;
	expect: typeof expect;
	writeShell(data: string | Uint8Array): Promise<void>;
	resizeShell(cols: number, rows: number): void;
	waitForScreen(text: string, timeoutMs?: number): Promise<void>;
	waitShellStatus(timeoutMs?: number): Promise<number | "timeout" | "error">;
	settle(): Promise<void>;
	screen(): string;
	currentLine(): string;
	markerLine(prefix: string): string | undefined;
	rawHex(): string;
	snapshot(suffix: string): string;
}

interface Case {
	id: string;
	knownBroken: boolean;
	ttyDependent: boolean;
	cols?: number;
	rows?: number;
	run(ctx: Ctx): Promise<void>;
}

// ---------------------------------------------------------------------------
// CASES — one shared table, looped over both runtimes
// ---------------------------------------------------------------------------

const CASES: Case[] = [
	{
		// Cooked-mode terminal echo: typing "abc" (no newline) must paint "abc" on
		// screen via the kernel ECHO flag while the program is still blocked in
		// read(). Correct on both runtimes: wasm-c surfaces the kernel echo via the
		// PTY master, and guest-node now routes stdin through the same kernel PTY.
		id: "cooked-echo",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=echo");
			await ctx.writeShell("abc");
			await ctx.settle();
			ctx.snapshot("echo-while-blocked");
			// Load-bearing honest echo assertion (kernel echo, NOT readback).
			ctx.expect(ctx.screen()).toContain("abc");
			await ctx.writeShell("\n");
			await ctx.waitForScreen("#BYTES tag=echo");
			ctx.snapshot("after-newline");
			ctx.expect(ctx.screen()).toContain(
				"#BYTES tag=echo n=4 hex=61 62 63 0A text=abc\\n",
			);
			await ctx.waitForScreen("#DONE id=cooked-echo");
		},
	},
	{
		// ECHOCTL: a non-signal control byte (^A 0x01) should echo as caret "^A".
		// The kernel caret-echoes control chars; correct on both runtimes now that
		// guest-node stdin flows through the kernel PTY.
		id: "control-char-echo",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=ctl");
			await ctx.writeShell("\x01");
			await ctx.settle();
			ctx.snapshot("echo");
			ctx.expect(ctx.screen()).toContain("^A");
			await ctx.writeShell("\n");
			await ctx.waitForScreen("#BYTES tag=ctl");
			ctx.snapshot("report");
			ctx.expect(ctx.screen()).toContain(
				"#BYTES tag=ctl n=2 hex=01 0A text=\\x01\\n",
			);
		},
	},
	{
		// RAW mode must not echo. Correct on both runtimes: guest-node's stdin is a
		// real PTY now, so setRawMode() succeeds (rc=0) and disables echo just like
		// the wasm-c probe.
		id: "raw-no-echo",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=raw");
			await ctx.writeShell("abc");
			await ctx.settle();
			const blocked = ctx.snapshot("blocked");
			ctx.expect(blocked).not.toContain("abc");
			ctx.expect(ctx.screen()).toContain("#MODE want=raw rc=0");
			await ctx.writeShell("!");
			await ctx.waitForScreen("#BYTES tag=raw");
			ctx.snapshot("done");
			ctx.expect(ctx.screen()).toContain(
				"#BYTES tag=raw n=4 hex=61 62 63 21 text=abc!",
			);
			await ctx.waitForScreen("#DONE id=raw-no-echo");
		},
	},
	{
		// VERASE (DEL 0x7f) in cooked mode erases one char (08 20 08 echo) and the
		// delivered line drops it. Correct on both runtimes (guest-node stdin is
		// cooked by the kernel PTY).
		id: "backspace",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=erase");
			await ctx.writeShell("ab");
			await ctx.writeShell(Uint8Array.of(0x7f));
			await ctx.settle();
			ctx.snapshot("echo");
			await ctx.writeShell("\n");
			await ctx.waitForScreen("#BYTES tag=erase");
			ctx.snapshot("report");
			// Load-bearing: VERASE drops the last buffered char, so the delivered
			// line is "a\n" (n=2), independent of the broken cooked screen echo.
			ctx.expect(ctx.markerLine("#BYTES tag=erase")).toBe(
				"#BYTES tag=erase n=2 hex=61 0A text=a\\n",
			);
		},
	},
	{
		// VKILL (^U 0x15) should clear the whole line. Implemented in the kernel
		// (clears the canonical line buffer + echoes the erase). Correct on both
		// runtimes now that guest-node stdin flows through the kernel PTY.
		id: "kill-line",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=kill");
			await ctx.writeShell("abc");
			await ctx.writeShell("\x15");
			await ctx.settle();
			ctx.snapshot("after-kill");
			ctx.expect(ctx.screen()).not.toContain("abc");
			await ctx.writeShell("\n");
			await ctx.waitForScreen("#BYTES tag=kill");
			ctx.snapshot("report");
			ctx.expect(ctx.screen()).toContain("#BYTES tag=kill n=1 hex=0A text=\\n");
		},
	},
	{
		// VWERASE (^W 0x17) should erase the last word. Implemented in the kernel
		// (erases the trailing non-blank run, keeping the preceding space). Correct
		// on both runtimes now that guest-node stdin flows through the kernel PTY.
		id: "word-erase",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=werase");
			await ctx.writeShell("foo bar\x17");
			await ctx.settle();
			ctx.snapshot("echo");
			ctx.expect(ctx.screen()).not.toContain("foo bar");
			await ctx.writeShell("\n");
			await ctx.waitForScreen("#BYTES tag=werase");
			ctx.snapshot("report");
			ctx.expect(ctx.screen()).toContain(
				"#BYTES tag=werase n=5 hex=66 6F 6F 20 0A text=foo \\n",
			);
		},
	},
	{
		// ICANON line buffering: read stays blocked (no #BYTES) until '\n', then
		// the whole line is delivered. EMPIRICALLY this delivery path works for
		// BOTH runtimes (the kernel buffers + delivers the cooked line to guest-node
		// stdin too), so we assert only buffering+delivery, not the broken echo.
		id: "line-buffering",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=canon");
			await ctx.writeShell("hello");
			await ctx.settle();
			const held = ctx.snapshot("held");
			// Load-bearing: kernel is HOLDING the line (no read returned yet).
			ctx.expect(held).not.toContain("#BYTES tag=canon");
			await ctx.writeShell("\n");
			await ctx.waitForScreen("#BYTES tag=canon");
			ctx.snapshot("delivered");
			ctx.expect(ctx.screen()).toContain(
				"#BYTES tag=canon n=6 hex=68 65 6C 6C 6F 0A text=hello\\n",
			);
		},
	},
	{
		// ISIG VINTR (^C 0x03) raises SIGINT to the foreground pgid; the byte is
		// neither delivered nor echoed. Both runtimes prove this via process
		// death: guest-node stdin is now a real kernel PTY (slave on fd 0), so
		// the discipline consumes the byte as a signal and SIGINT terminates the
		// shell process exactly like wasm-c.
		id: "sigint",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=sigint");
			await ctx.writeShell("\x03");
			const status = await ctx.waitShellStatus();
			ctx.snapshot("after");
			// Correct: VINTR is consumed as a signal -> the byte is neither echoed
			// nor delivered (no #BYTES) and the probe is killed mid-read so it never
			// reaches #DONE.
			ctx.expect(ctx.screen()).not.toContain("#BYTES tag=sigint");
			ctx.expect(ctx.screen()).not.toContain("#DONE id=sigint");
			ctx.expect(ctx.screen()).not.toContain("^C");
			// POSITIVE proof of death (fixes the prior weakness where a merely-HUNG
			// process — signal silently dropped — also satisfied the negatives above):
			// waitShell RESOLVED, so SIGINT actually TERMINATED the process instead of
			// the read blocking the full 15s — `status` is a real exit status, never
			// "timeout"/"error". Combined with the missing #DONE that means it died
			// mid-read. (The wasm PTY-signal kill surfaces a racy/zero exit code, so
			// death is proven by termination + no #DONE, not a 128+sig code.) guest-node
			// survives the signal and reaches #DONE via EOF, so `not #DONE` throws and
			// the cell stays correctly RED under it.fails until signal->isolate lands.
			ctx.expect(status).not.toBe("timeout");
			ctx.expect(status).not.toBe("error");
		},
	},
	{
		// ISIG VQUIT (^\ 0x1C) raises SIGQUIT. Same shape as sigint: the byte is
		// consumed as a signal (no #BYTES, no echo) and the signal terminates the
		// shell process on both runtimes.
		id: "sigquit",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=sigquit");
			await ctx.writeShell(Uint8Array.of(0x1c));
			const status = await ctx.waitShellStatus();
			ctx.snapshot("after");
			// Correct: VQUIT is consumed as a signal (no #BYTES, no #DONE).
			ctx.expect(ctx.screen()).not.toContain("#BYTES tag=sigquit");
			ctx.expect(ctx.screen()).not.toContain("#DONE id=sigquit");
			// POSITIVE proof of death: waitShell RESOLVED, so SIGQUIT terminated the
			// process (status is never a 15s "timeout"); with the missing #DONE that
			// means it died mid-read. (js-node survives -> reaches #DONE -> the `not
			// #DONE` check throws -> stays correctly RED under it.fails.)
			ctx.expect(status).not.toBe("timeout");
			ctx.expect(status).not.toBe("error");
		},
	},
	{
		// RAW mode (ISIG off): VINTR (0x03) is ordinary data, not a signal. The
		// observable (#BYTES n=1 hex=03) is identical on both runtimes -> green.
		id: "raw-ctrlc-byte",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=rawc");
			ctx.snapshot("before-input");
			await ctx.writeShell("\x03");
			await ctx.waitForScreen("#BYTES tag=rawc");
			ctx.snapshot("after");
			ctx.expect(ctx.screen()).toContain("#BYTES tag=rawc n=1 hex=03 text=\\x03");
			ctx.expect(ctx.screen()).not.toContain("^C");
			await ctx.waitForScreen("#DONE id=raw-ctrlc-byte");
		},
	},
	{
		// ISIG VSUSP (^Z 0x1a) raises SIGTSTP. The kernel line discipline consumes
		// the byte as a signal on BOTH runtimes (pure pty.rs behavior): it is never
		// echoed (no caret) and never delivered (no #BYTES), and the cooked read
		// never completes into a clean #DONE. (We assert only the line-discipline
		// contract here, not the host-side suspension: SIGTSTP marks the process
		// Stopped rather than killing it, so unlike sigint there is no exit code to
		// assert — proving the byte is swallowed as a signal is the conformant check
		// and it holds identically for wasm-c and guest-node.)
		id: "vsusp",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=susp");
			await ctx.writeShell(Uint8Array.of(0x1a));
			await ctx.settle();
			ctx.snapshot("after");
			ctx.expect(ctx.screen()).not.toContain("#BYTES tag=susp");
			ctx.expect(ctx.screen()).not.toContain("#DONE id=vsusp");
			// Not echoed: neither the raw control byte nor an ECHOCTL caret ("^Z").
			ctx.expect(ctx.screen()).not.toContain("^Z");
			ctx.expect(ctx.screen()).not.toContain("\\x1A");
		},
	},
	{
		// VERASE alias ^H (0x08): the kernel maps both DEL (0x7f) and ^H (0x08) to
		// the erase op, so typing "ab" + ^H + '\n' drops the last buffered char and
		// delivers "a\n". Pure line discipline -> correct on both runtimes.
		id: "erase-ctrl-h",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=eraseh");
			await ctx.writeShell("ab");
			await ctx.writeShell(Uint8Array.of(0x08));
			await ctx.settle();
			ctx.snapshot("echo");
			await ctx.writeShell("\n");
			await ctx.waitForScreen("#BYTES tag=eraseh");
			ctx.snapshot("report");
			// Load-bearing: ^H erased the last buffered byte exactly like DEL, so the
			// delivered line is "a\n" (n=2), independent of the screen echo.
			ctx.expect(ctx.markerLine("#BYTES tag=eraseh")).toBe(
				"#BYTES tag=eraseh n=2 hex=61 0A text=a\\n",
			);
		},
	},
	{
		// VINTR mid-line BOTH flushes the canonical input buffer AND raises SIGINT.
		// Neither runtime can catch SIGINT (both are killed mid-read), so this
		// cell observes the kill half on both: "abc" is buffered but never
		// delivered (no #BYTES), the probe never completes (no #DONE), and the
		// shell terminates instead of blocking the full 15s. The buffer-flush
		// half is covered by the kernel pty unit tests (canonical buffer cleared
		// on VINTR), which don't need a signal-surviving guest to observe it.
		id: "vintr-buffer",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=vintrbuf");
			await ctx.writeShell("abc");
			await ctx.settle();
			await ctx.writeShell("\x03");
			await ctx.writeShell("de\n");
			const status = await ctx.waitShellStatus();
			ctx.snapshot("after");
			// Killed by SIGINT mid-read: "abc" is never delivered (no #BYTES) and
			// the probe never completes (no #DONE). waitShell RESOLVED (not a 15s
			// "timeout"), proving the signal actually terminated it.
			ctx.expect(ctx.screen()).not.toContain("#BYTES tag=vintrbuf");
			ctx.expect(ctx.screen()).not.toContain("#DONE id=vintr-buffer");
			ctx.expect(status).not.toBe("timeout");
			ctx.expect(status).not.toBe("error");
		},
	},
	{
		// OPOST+ONLCR: a raw-written lone LF must become CRLF on the master read
		// path. Fixed on wasm-c: the host now surfaces the PTY master output
		// (ONLCR-processed) instead of the raw guest chunk, so the master reads
		// 61 0D 0A 62. js-node's raw stdout payload write does not flow through the
		// ONLCR path (root cause 3 js stdout quirk) -> still broken -> tty-dependent.
		id: "onlcr",
		knownBroken: false,
		ttyDependent: true,
		async run(ctx) {
			await ctx.waitForScreen("#DONE id=onlcr");
			ctx.snapshot("onlcr");
			ctx.expect(ctx.rawHex()).toContain("61 0D 0A 62");
			ctx.expect(ctx.rawHex()).not.toContain("61 0A 62");
		},
	},
	{
		// ICRNL maps a typed CR (0x0D) to NL (0x0A) in cooked mode: it both
		// terminates the line and is the byte delivered. Correct on both runtimes
		// (guest-node stdin is cooked by the kernel PTY).
		id: "icrnl",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=icrnl");
			await ctx.writeShell("x");
			await ctx.settle();
			const echoSnap = ctx.snapshot("echo");
			// Read still blocked (no terminator yet): the line is held by canonical
			// mode, so the delivery marker must not be present.
			ctx.expect(echoSnap).not.toContain("#BYTES tag=icrnl");
			await ctx.writeShell("\r");
			await ctx.waitForScreen("#BYTES tag=icrnl");
			ctx.snapshot("final");
			// Load-bearing: the typed CR (0x0D) was mapped by ICRNL to NL (0x0A),
			// terminating the line AND being the byte delivered -> "x\n" (78 0A).
			ctx.expect(ctx.screen()).toContain(
				"#BYTES tag=icrnl n=2 hex=78 0A text=x\\n",
			);
		},
	},
	{
		// VEOF (^D 0x04) on an empty line in cooked mode -> read() returns 0 (EOF),
		// not echoed. Correct on both runtimes: guest-node reads the kernel PTY,
		// so ^D surfaces as a 0-length read (EOF), not a data byte.
		id: "eof",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#READY tag=eof");
			ctx.snapshot("ready");
			await ctx.writeShell(Uint8Array.of(0x04));
			await ctx.waitForScreen("#DONE id=eof");
			ctx.snapshot("done");
			// ^D never echoed on either runtime.
			ctx.expect(ctx.screen()).not.toContain("^D");
			ctx.expect(ctx.screen()).not.toContain("\\x04");
			// Correct: clean EOF, no data byte delivered.
			ctx.expect(ctx.screen()).toContain("#EOF tag=eof n=0");
			ctx.expect(ctx.screen()).not.toContain("#BYTES tag=eof");
		},
	},
	{
		// SIGWINCH / live window size: after the host resizes the PTY, the probe
		// re-queries and must see the NEW size. The native sidecar forwards the
		// kernel resize as SIGWINCH to embedded V8 so js-node re-queries it.
		id: "resize-sigwinch",
		knownBroken: false,
		ttyDependent: false,
		cols: 80,
		rows: 24,
		async run(ctx) {
			await ctx.waitForScreen("#SIZE tag=before");
			await ctx.waitForScreen("#READY tag=resize");
			ctx.resizeShell(120, 40);
			if (ctx.runtime.name === "js-node") {
				// Keep the probe alive until the asynchronous SIGWINCH handler has
				// queried the resized PTY. Releasing it first races process.exit(0)
				// against the coalesced signal wake.
				await ctx.waitForScreen("#SIZE tag=after rc=0 cols=120 rows=40");
			}
			// Sentinel '!' + CR (ICRNL -> NL flushes the cooked line). WASM has
			// no signal handler, so it re-queries only after this read completes.
			await ctx.writeShell("!\r");
			await ctx.waitForScreen("#SIZE tag=after rc=0 cols=120 rows=40");
			ctx.snapshot("resize");
			ctx.expect(ctx.screen()).toContain(
				"#SIZE tag=before rc=0 cols=80 rows=24",
			);
		},
	},
	{
		// Cursor Position Report round-trip: probe writes ESC[6n and reads the
		// emulator's ESC[<r>;<c>R reply. Works on BOTH runtimes (guest-node stdin
		// is a non-canonical data stream, so the newline-less reply still flows);
		// only the #MODE marker diverges, which we do not assert.
		id: "cpr",
		knownBroken: false,
		ttyDependent: false,
		async run(ctx) {
			await ctx.waitForScreen("#CPR sent=1");
			await ctx.waitForScreen("#CPRREPLY");
			ctx.snapshot("cpr-reply");
			const m = ctx
				.screen()
				.match(/#CPRREPLY n=\d+ hex=[0-9A-F ]+ text=(\S+)/);
			ctx.expect(m, "expected a #CPRREPLY marker line").toBeTruthy();
			ctx.expect(m?.[1] ?? "").toMatch(/^\\e\[\d+;\d+R$/);
		},
	},
	{
		// isatty(0/1/2): a PTY slave dup2'd onto the fds reports TTY for all three.
		// Correct on both runtimes: guest-node's TTY config is now derived from the
		// kernel isatty bridge, so process.stdin/stdout/stderr.isTTY are all true.
		id: "isatty",
		knownBroken: false,
		ttyDependent: false,
		cols: 80,
		rows: 24,
		async run(ctx) {
			await ctx.waitForScreen("#TTY ");
			ctx.snapshot("tty");
			ctx.expect(ctx.markerLine("#TTY ")).toBe("#TTY in=1 out=1 err=1");
			await ctx.waitForScreen("#DONE id=isatty");
		},
	},
	{
		// Window size: the kernel slave winsize must match openShell({cols,rows}).
		// Correct on both runtimes: guest-node's process.stdout.columns/rows are now
		// read live from the kernel PTY winsize. Non-default dims so a stale 80x24
		// fallback can't pass by accident.
		id: "winsize",
		knownBroken: false,
		ttyDependent: false,
		cols: 100,
		rows: 37,
		async run(ctx) {
			await ctx.waitForScreen("#SIZE tag=open");
			await ctx.waitForScreen("#DONE id=winsize");
			ctx.snapshot("winsize");
			ctx.expect(ctx.screen()).toContain(
				"#SIZE tag=open rc=0 cols=100 rows=37",
			);
		},
	},
];

const RUNTIMES: Runtime[] = [
	...(ENABLE_WASM_C_PTY ? ([{ name: "wasm-c" }] as const) : []),
	{ name: "js-node" },
];

// ---------------------------------------------------------------------------
// suite
// ---------------------------------------------------------------------------

describe("PTY line discipline matrix", () => {
	let vm: AgentOs | undefined;

	beforeAll(async () => {
		ensureSidecarBuilt();

		const { AgentOs } = await import("../src/index.js");
		const software = [];
		if (ENABLE_WASM_C_PTY) {
			software.push({ packagePath: materializePtyProbePackage() });
		}
		vm = await AgentOs.create({
			software,
		});
		await vm.writeFile(NODE_PROBE_GUEST_PATH, readFileSync(NODE_PROBE_SOURCE));
	}, 180_000);

	afterAll(async () => {
		if (vm) {
			await vm.dispose();
			vm = undefined;
		}
	});

	for (const rt of RUNTIMES) {
		describe(rt.name, () => {
			let term: Terminal | undefined;
			let shellId: string | undefined;
			let unsubscribe: (() => void) | undefined;
			let disposeOnData: { dispose(): void } | undefined;
			let rawBytes: number[] = [];

			afterEach(async () => {
				unsubscribe?.();
				unsubscribe = undefined;
				disposeOnData?.dispose();
				disposeOnData = undefined;
				if (vm && shellId) {
					try {
						vm.closeShell(shellId);
					} catch {
						// probe may already have exited
					}
				}
				term?.dispose();
				term = undefined;
				shellId = undefined;
				rawBytes = [];
			});

			for (const c of CASES) {
				const effectiveKnownBroken =
					c.knownBroken || (rt.name === "js-node" && c.ttyDependent);
				registerCase(rt, c, effectiveKnownBroken, {
					getVm: () => vm,
					setState: (t, sid, un, dod) => {
						term = t;
						shellId = sid;
						unsubscribe = un;
						disposeOnData = dod;
					},
					getRawBytes: () => rawBytes,
				});
			}
		});
	}
});

// ---------------------------------------------------------------------------
// per-cell registration (kept out of the loop body for clarity)
// ---------------------------------------------------------------------------

interface CellHooks {
	getVm(): AgentOs | undefined;
	setState(
		term: Terminal,
		shellId: string,
		unsubscribe: () => void,
		disposeOnData: { dispose(): void },
	): void;
	getRawBytes(): number[];
}

function registerCase(
	rt: Runtime,
	c: Case,
	effectiveKnownBroken: boolean,
	hooks: CellHooks,
): void {
	const runner = effectiveKnownBroken ? it.fails : it;
	const title = effectiveKnownBroken ? `${c.id} [known-broken]` : c.id;

	runner(
		title,
		async () => {
			const vm = hooks.getVm();
			if (!vm) throw new Error("vm not initialized");

			const cols = c.cols ?? 80;
			const rows = c.rows ?? 24;
			const term = new Terminal({ cols, rows, allowProposedApi: true });
			const rawBytes = hooks.getRawBytes();

			const command =
				rt.name === "wasm-c" ? "pty_probe" : "node";
			const args =
				rt.name === "wasm-c"
					? [c.id]
					: [NODE_PROBE_GUEST_PATH, c.id];

			const { shellId } = vm.openShell({
				command,
				args,
				cols,
				rows,
				env: {
					TERM: "xterm-256color",
					COLUMNS: String(cols),
					LINES: String(rows),
				},
			});

			const unsubscribe = vm.onShellData(shellId, (data) => {
				for (let i = 0; i < data.length; i++) rawBytes.push(data[i]);
				term.write(data);
			});
			const disposeOnData = term.onData((data) => {
				vm.writeShell(shellId, data);
			});
			hooks.setState(term, shellId, unsubscribe, disposeOnData);

			const ctx: Ctx = {
				runtime: rt,
				broken: effectiveKnownBroken,
				vm,
				shellId,
				term,
				expect,
				async writeShell(data) {
					await vm.writeShell(shellId, data);
				},
				resizeShell(c2, r2) {
					term.resize(c2, r2);
					vm.resizeShell(shellId, c2, r2);
				},
				async waitForScreen(text, timeoutMs = WAIT_TIMEOUT_MS) {
					const deadline = Date.now() + timeoutMs;
					while (Date.now() < deadline) {
						if (screenText(term).includes(text)) {
							await settle();
							return;
						}
						await new Promise((r) => setTimeout(r, 25));
					}
					throw new Error(
						`timed out waiting for ${JSON.stringify(text)}\n${terminalSnapshot(
							"timeout",
							term,
						)}`,
					);
				},
				async waitShellStatus(timeoutMs = WAIT_TIMEOUT_MS) {
					return await Promise.race<number | "timeout" | "error">([
						vm.waitShell(shellId).then(
							(s) => s,
							() => "error" as const,
						),
						new Promise<"timeout">((r) =>
							setTimeout(() => r("timeout"), timeoutMs),
						),
					]);
				},
				settle,
				screen() {
					return screenText(term);
				},
				currentLine() {
					const buffer = term.buffer.active;
					const line = buffer.getLine(buffer.cursorY + buffer.viewportY);
					return (line ? line.translateToString(true) : "").replace(/\s+$/, "");
				},
				markerLine(prefix) {
					for (const line of snapshotLines(term)) {
						const idx = line.indexOf(prefix);
						if (idx !== -1) return line.slice(idx).replace(/\s+$/, "");
					}
					return undefined;
				},
				rawHex() {
					return rawBytes
						.map((b) => b.toString(16).toUpperCase().padStart(2, "0"))
						.join(" ");
				},
				snapshot(suffix) {
					const label = `${rt.name}/${c.id}/${suffix}`;
					const snap = terminalSnapshot(label, term);
					// CRITICAL: a known-broken (it.fails) cell must throw ONLY from its
					// load-bearing behavior assertion, never from a stale snapshot. If we
					// asserted the snapshot here, a future FIX would change the screen, the
					// snapshot would mismatch and throw, and it.fails would stay green —
					// masking the very fix it should flag. So for it.fails cells the
					// snapshot is captured for review but NOT asserted; the behavior
					// assertion is the sole arbiter, so a fix turns the cell RED.
					if (!effectiveKnownBroken) {
						expect(snap).toMatchSnapshot(label);
					}
					return snap;
				},
			};

			await c.run(ctx);
		},
		TEST_TIMEOUT_MS,
	);
}
