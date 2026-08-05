// Server-side terminal state for shell replay.
//
// A shell outlives the page that opened it, and the dashboard reboots the tab
// iframe on every tab switch. Without state on this side, a reattaching client
// sees a blank pane until the guest happens to repaint — which a full-screen
// program (vim, less) never does unprompted. So the actor keeps a headless
// Ghostty emulator per shell, fed from the same chunks it broadcasts, and
// serializes it back to ANSI on demand.
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import type { ShellReplayMode, ShellSnapshot } from "./types.js";

/** Emulators alive at once. Matches the actor's open-shell ceiling. */
const MAX_REPLAY_SHELLS = 32;
/** Scrollback retained per shell. Bounds the `scrollback` snapshot mode and
 * the emulator's own memory; the screen itself is bounded by cols × rows. */
const MAX_SCROLLBACK_LINES = 1_000;
/** Rejects an absurd resize before it becomes a cols × rows allocation. */
const MAX_COLS = 1_000;
const MAX_ROWS = 1_000;

const DEFAULT_COLS = 80;
const DEFAULT_ROWS = 24;

// ── WASM loading ───────────────────────────────────────────────────────────
// ghostty-web's own loader resolves its inlined wasm against `self.location`,
// which does not exist outside a browser. Instantiating the shipped .wasm
// directly avoids both the missing global and a multi-megabyte base64 decode.
interface GhosttyModule {
	createTerminal(
		cols: number,
		rows: number,
		config?: { scrollbackLimit?: number },
	): HeadlessTerminal;
}

interface GhosttyCell {
	codepoint: number;
	fg_r: number;
	fg_g: number;
	fg_b: number;
	bg_r: number;
	bg_g: number;
	bg_b: number;
	flags: number;
	width: number;
	grapheme_len: number;
}

interface HeadlessTerminal {
	write(data: string | Uint8Array): void;
	resize(cols: number, rows: number): void;
	free(): void;
	update(): number;
	getViewport(): GhosttyCell[];
	getCursor(): { viewportX: number; viewportY: number; visible: number };
	getColors(): {
		foreground: { r: number; g: number; b: number };
		background: { r: number; g: number; b: number };
	};
	getGraphemeString(row: number, col: number): string;
	getScrollbackLength(): number;
	getScrollbackLine(offset: number): GhosttyCell[] | null;
	getScrollbackGraphemeString(offset: number, col: number): string;
	isAlternateScreen(): boolean;
	getMode(mode: number, isAnsi?: boolean): boolean;
}

let ghosttyPromise: Promise<GhosttyModule> | undefined;

async function loadGhostty(): Promise<GhosttyModule> {
	ghosttyPromise ??= (async () => {
		const require = createRequire(import.meta.url);
		const { Ghostty } = await import("ghostty-web");
		const bytes = await readFile(
			require.resolve("ghostty-web/ghostty-vt.wasm"),
		);
		const { instance } = await WebAssembly.instantiate(bytes, {
			env: { log: () => {} },
		});
		return new Ghostty(instance) as unknown as GhosttyModule;
	})();
	return ghosttyPromise;
}

// ── ANSI serialization ─────────────────────────────────────────────────────
const CELL_BOLD = 1;
const CELL_ITALIC = 2;
const CELL_UNDERLINE = 4;
const CELL_STRIKETHROUGH = 8;
const CELL_INVERSE = 16;
const CELL_INVISIBLE = 32;
const CELL_BLINK = 64;
const CELL_FAINT = 128;

/** DEC private modes worth restoring: getting these wrong strands the guest in
 * a state its client no longer agrees with — arrow keys emitting the wrong
 * prefix, or a paste arriving unbracketed and executing as commands. Each is
 * read back individually rather than inferred, so the exact mouse protocol
 * survives too. Alt-screen (1049) and cursor visibility (25) are restored
 * separately, in a fixed order relative to the repaint. */
const RESTORED_DEC_MODES = [
	1, // DECCKM — application cursor keys
	7, // DECAWM — autowrap
	1000, // mouse: click tracking
	1002, // mouse: button-event tracking
	1003, // mouse: any-event tracking
	1004, // focus in/out reporting
	1005, // mouse: UTF-8 coordinates
	1006, // mouse: SGR coordinates
	1015, // mouse: urxvt coordinates
	2004, // bracketed paste
];

interface CellStyle {
	fg: number | null;
	bg: number | null;
	flags: number;
}

const DEFAULT_STYLE: CellStyle = { fg: null, bg: null, flags: 0 };

function rgb(r: number, g: number, b: number): number {
	return (r << 16) | (g << 8) | b;
}

function styleOf(
	cell: GhosttyCell,
	defaultFg: number,
	defaultBg: number,
): CellStyle {
	const fg = rgb(cell.fg_r, cell.fg_g, cell.fg_b);
	const bg = rgb(cell.bg_r, cell.bg_g, cell.bg_b);
	return {
		fg: fg === defaultFg ? null : fg,
		bg: bg === defaultBg ? null : bg,
		flags: cell.flags,
	};
}

function sameStyle(a: CellStyle, b: CellStyle): boolean {
	return a.fg === b.fg && a.bg === b.bg && a.flags === b.flags;
}

/** Full SGR for a style, always prefixed with a reset so it never inherits
 * attributes from whatever the client last rendered. */
function sgr(style: CellStyle): string {
	const params: number[] = [0];
	if (style.flags & CELL_BOLD) params.push(1);
	if (style.flags & CELL_FAINT) params.push(2);
	if (style.flags & CELL_ITALIC) params.push(3);
	if (style.flags & CELL_UNDERLINE) params.push(4);
	if (style.flags & CELL_BLINK) params.push(5);
	if (style.flags & CELL_INVERSE) params.push(7);
	if (style.flags & CELL_INVISIBLE) params.push(8);
	if (style.flags & CELL_STRIKETHROUGH) params.push(9);
	if (style.fg !== null) {
		params.push(
			38,
			2,
			(style.fg >> 16) & 0xff,
			(style.fg >> 8) & 0xff,
			style.fg & 0xff,
		);
	}
	if (style.bg !== null) {
		params.push(
			48,
			2,
			(style.bg >> 16) & 0xff,
			(style.bg >> 8) & 0xff,
			style.bg & 0xff,
		);
	}
	return `\x1b[${params.join(";")}m`;
}

/** One row of cells → text plus SGR changes. `graphemeAt` supplies the full
 * cluster for cells that carry combining marks. Trailing default-styled blanks
 * are dropped in favour of an erase-to-end-of-line. */
function serializeRow(
	cells: GhosttyCell[],
	cols: number,
	defaultFg: number,
	defaultBg: number,
	graphemeAt: (col: number) => string,
): string {
	let lastMeaningful = -1;
	for (let x = 0; x < cols; x++) {
		const cell = cells[x];
		if (!cell) continue;
		const blank = cell.codepoint === 0 || cell.codepoint === 32;
		if (
			!blank ||
			!sameStyle(styleOf(cell, defaultFg, defaultBg), DEFAULT_STYLE)
		) {
			lastMeaningful = x;
		}
	}
	if (lastMeaningful < 0) return "\x1b[0m\x1b[K";

	let out = "";
	let current = DEFAULT_STYLE;
	out += sgr(current);
	for (let x = 0; x <= lastMeaningful; x++) {
		const cell = cells[x];
		if (!cell) {
			out += " ";
			continue;
		}
		// Width 0 is the trailing half of a wide character; the preceding cell
		// already emitted the whole glyph.
		if (cell.width === 0) continue;
		const style = styleOf(cell, defaultFg, defaultBg);
		if (!sameStyle(style, current)) {
			out += sgr(style);
			current = style;
		}
		if (cell.grapheme_len > 0) {
			out += graphemeAt(x);
		} else if (cell.codepoint === 0) {
			out += " ";
		} else {
			out += String.fromCodePoint(cell.codepoint);
		}
	}
	return `${out}\x1b[0m\x1b[K`;
}

// ── Store ──────────────────────────────────────────────────────────────────
interface ReplayEntry {
	term: HeadlessTerminal;
	cols: number;
	rows: number;
	seq: number;
}

function clamp(
	value: number | undefined,
	fallback: number,
	max: number,
): number {
	if (!Number.isFinite(value) || value === undefined || value <= 0)
		return fallback;
	return Math.min(Math.floor(value), max);
}

/**
 * One headless emulator per live shell, fed the same bytes the actor
 * broadcasts. All methods are no-ops for unknown shell ids so a shell opened
 * before replay existed (or one whose emulator failed to start) degrades to
 * the previous behaviour instead of failing the write path.
 */
export class ShellReplayStore {
	private entries = new Map<string, ReplayEntry>();
	private ghostty: GhosttyModule | null = null;

	/** Instantiates the WASM module. Callers must await this before opening a
	 * shell so that `open` itself can be synchronous: a PTY starts producing
	 * output the moment it exists, and any await between opening it and
	 * subscribing is a window where output is lost to every consumer. */
	async warm(): Promise<void> {
		this.ghostty ??= await loadGhostty();
	}

	open(shellId: string, cols?: number, rows?: number): void {
		if (this.entries.has(shellId)) return;
		if (this.entries.size >= MAX_REPLAY_SHELLS) {
			throw new Error(
				`agent-os shell replay limit reached (${MAX_REPLAY_SHELLS} emulators); close a shell or raise MAX_REPLAY_SHELLS in shell-replay.ts`,
			);
		}
		if (!this.ghostty) {
			throw new Error("agent-os shell replay used before warm() resolved");
		}
		const width = clamp(cols, DEFAULT_COLS, MAX_COLS);
		const height = clamp(rows, DEFAULT_ROWS, MAX_ROWS);
		const term = this.ghostty.createTerminal(width, height, {
			scrollbackLimit: MAX_SCROLLBACK_LINES,
		});
		// A freed terminal's handle is recycled without its screen being cleared,
		// so a new emulator can start holding the previous shell's contents — and
		// serve them to whoever attaches next. Reset before anything is fed in.
		term.write("\x1bc");
		this.entries.set(shellId, { term, cols: width, rows: height, seq: 0 });
	}

	/** Feeds one broadcast chunk and returns its sequence number. Clients use
	 * it to drop chunks a snapshot already contains. Returns 0 when this shell
	 * has no emulator, which clients read as "no replay available". */
	feed(shellId: string, data: Uint8Array): number {
		const entry = this.entries.get(shellId);
		if (!entry) return 0;
		entry.term.write(data);
		return ++entry.seq;
	}

	resize(shellId: string, cols: number, rows: number): void {
		const entry = this.entries.get(shellId);
		if (!entry) return;
		entry.cols = clamp(cols, entry.cols, MAX_COLS);
		entry.rows = clamp(rows, entry.rows, MAX_ROWS);
		entry.term.resize(entry.cols, entry.rows);
	}

	close(shellId: string): void {
		const entry = this.entries.get(shellId);
		if (!entry) return;
		this.entries.delete(shellId);
		entry.term.free();
	}

	has(shellId: string): boolean {
		return this.entries.has(shellId);
	}

	snapshot(shellId: string, mode: ShellReplayMode): ShellSnapshot | null {
		const entry = this.entries.get(shellId);
		if (!entry) return null;
		const { term, cols, rows, seq } = entry;
		const base = { shellId, seq, cols, rows };
		if (mode === "none") return { ...base, mode, data: "" };

		term.update();
		const colors = term.getColors();
		const defaultFg = rgb(
			colors.foreground.r,
			colors.foreground.g,
			colors.foreground.b,
		);
		const defaultBg = rgb(
			colors.background.r,
			colors.background.g,
			colors.background.b,
		);
		const alternate = term.isAlternateScreen();

		// RIS first: the client pane may be reused, and a repaint that inherits
		// stale modes or attributes is worse than no repaint at all.
		let out = "\x1bc";

		// Scrollback is written first and scrolled off by the screen paint that
		// follows, so it lands in the client's scrollback rather than on screen.
		// It belongs to the primary screen, so it precedes the alt-screen switch.
		if (mode === "scrollback" && !alternate) {
			const available = Math.min(
				term.getScrollbackLength(),
				MAX_SCROLLBACK_LINES,
			);
			for (let i = available; i > 0; i--) {
				const offset = i - 1;
				const line = term.getScrollbackLine(offset);
				if (!line) continue;
				out += serializeRow(line, cols, defaultFg, defaultBg, (col) =>
					term.getScrollbackGraphemeString(offset, col),
				);
				out += "\r\n";
			}
		}

		// Modes go on before the repaint so the client is already in the right
		// screen when cells land; 1049 also clears, which would undo the paint.
		if (alternate) out += "\x1b[?1049h";
		for (const decMode of RESTORED_DEC_MODES) {
			out += term.getMode(decMode) ? `\x1b[?${decMode}h` : `\x1b[?${decMode}l`;
		}

		// Rows are painted sequentially from home rather than by absolute
		// address: each `\r\n` scrolls whatever preceded it into scrollback,
		// which is what carries the dump above off-screen intact.
		const viewport = term.getViewport();
		out += "\x1b[H";
		for (let y = 0; y < rows; y++) {
			if (y > 0) out += "\r\n";
			const row = viewport.slice(y * cols, (y + 1) * cols);
			out += serializeRow(row, cols, defaultFg, defaultBg, (col) =>
				term.getGraphemeString(y, col),
			);
		}

		const cursor = term.getCursor();
		const cy = Math.min(Math.max(cursor.viewportY, 0), rows - 1) + 1;
		const cx = Math.min(Math.max(cursor.viewportX, 0), cols - 1) + 1;
		out += `\x1b[0m\x1b[${cy};${cx}H`;
		out += cursor.visible ? "\x1b[?25h" : "\x1b[?25l";

		return { ...base, mode, data: out };
	}
}
