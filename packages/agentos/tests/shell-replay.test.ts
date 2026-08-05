import { describe, expect, test } from "vitest";
import { ShellReplayStore } from "../src/shell-replay.js";

const COLS = 40;
const ROWS = 10;

async function storeWith(...chunks: string[]): Promise<ShellReplayStore> {
	const store = new ShellReplayStore();
	await store.warm();
	store.open("shell-1", COLS, ROWS);
	for (const chunk of chunks) {
		store.feed("shell-1", new TextEncoder().encode(chunk));
	}
	return store;
}

/** Writes a snapshot into a fresh emulator and snapshots that. A faithful
 * serialization is a fixed point: replaying it must reproduce itself. */
async function replay(data: string): Promise<string> {
	const store = new ShellReplayStore();
	await store.warm();
	store.open("replayed", COLS, ROWS);
	store.feed("replayed", new TextEncoder().encode(data));
	const snapshot = store.snapshot("replayed", "screen");
	store.close("replayed");
	return snapshot?.data ?? "";
}

describe("shell replay", () => {
	test("repaints plain output", async () => {
		const store = await storeWith("hello\r\nworld\r\n");
		const snapshot = store.snapshot("shell-1", "screen");
		expect(snapshot).not.toBeNull();
		expect(snapshot?.data).toContain("hello");
		expect(snapshot?.data).toContain("world");
		expect(snapshot?.seq).toBe(1);
		expect(snapshot).toMatchObject({
			shellId: "shell-1",
			mode: "screen",
			cols: COLS,
			rows: ROWS,
		});
		store.close("shell-1");
	});

	test("round-trips through a fresh emulator", async () => {
		const store = await storeWith(
			"plain\r\n",
			"\x1b[31mred\x1b[0m \x1b[1;4mbold-underline\x1b[0m\r\n",
			"\x1b[48;2;10;20;30mrgb-bg\x1b[0m\r\n",
			"trailing spaces   \r\n",
			"unicode: héllo ✓\r\n",
		);
		const snapshot = store.snapshot("shell-1", "screen");
		expect(await replay(snapshot?.data ?? "")).toBe(snapshot?.data);
		store.close("shell-1");
	});

	test("preserves alternate screen and cursor position", async () => {
		const store = await storeWith("\x1b[?1049h", "\x1b[3;5Hin-alt-screen");
		const snapshot = store.snapshot("shell-1", "screen");
		expect(snapshot?.data).toContain("\x1b[?1049h");
		expect(snapshot?.data).toContain("in-alt-screen");
		// Cursor lands just past the text it wrote.
		expect(snapshot?.data).toContain("\x1b[3;18H");
		expect(await replay(snapshot?.data ?? "")).toBe(snapshot?.data);
		store.close("shell-1");
	});

	test("restores DEC modes a client would otherwise lose", async () => {
		const store = await storeWith("\x1b[?1h\x1b[?2004h\x1b[?1006h\x1b[?25l");
		const data = store.snapshot("shell-1", "screen")?.data ?? "";
		expect(data).toContain("\x1b[?1h");
		expect(data).toContain("\x1b[?2004h");
		expect(data).toContain("\x1b[?1006h");
		expect(data).toContain("\x1b[?25l");
		// Modes the guest never enabled are explicitly reset, not left to chance.
		expect(data).toContain("\x1b[?1003l");
		store.close("shell-1");
	});

	test("scrollback mode carries lines above the viewport", async () => {
		const lines = Array.from({ length: ROWS + 5 }, (_, i) => `line-${i}\r\n`);
		const store = await storeWith(...lines);
		const screen = store.snapshot("shell-1", "screen")?.data ?? "";
		const scrollback = store.snapshot("shell-1", "scrollback")?.data ?? "";
		expect(screen).not.toContain("line-0");
		expect(scrollback).toContain("line-0");
		expect(scrollback).toContain(`line-${ROWS + 4}`);
		store.close("shell-1");
	});

	test("`none` carries a sequence but no repaint", async () => {
		const store = await storeWith("output\r\n", "more\r\n");
		expect(store.snapshot("shell-1", "none")).toMatchObject({
			mode: "none",
			data: "",
			seq: 2,
		});
		store.close("shell-1");
	});

	test("sequence advances per chunk and identifies unknown shells", async () => {
		const store = await storeWith("a", "b", "c");
		expect(store.snapshot("shell-1", "none")?.seq).toBe(3);
		expect(store.feed("unknown", new Uint8Array([1]))).toBe(0);
		expect(store.snapshot("unknown", "screen")).toBeNull();
		store.close("shell-1");
		expect(store.snapshot("shell-1", "screen")).toBeNull();
	});

	test("resize is reflected in later snapshots", async () => {
		const store = await storeWith("resize me\r\n");
		store.resize("shell-1", 100, 30);
		expect(store.snapshot("shell-1", "screen")).toMatchObject({
			cols: 100,
			rows: 30,
		});
		store.close("shell-1");
	});

	test("open is bounded and names the limit", async () => {
		const store = new ShellReplayStore();
		await store.warm();
		for (let i = 0; i < 32; i++) store.open(`shell-${i}`, COLS, ROWS);
		expect(() => store.open("shell-32", COLS, ROWS)).toThrow(
			/replay limit reached \(32 emulators\).*MAX_REPLAY_SHELLS/s,
		);
		for (let i = 0; i < 32; i++) store.close(`shell-${i}`);
	});

	test("a reopened shell never inherits a closed shell's screen", async () => {
		const store = new ShellReplayStore();
		await store.warm();
		store.open("first", COLS, ROWS);
		store.feed(
			"first",
			new TextEncoder().encode("secret-from-first-shell\r\n"),
		);
		store.close("first");

		store.open("second", COLS, ROWS);
		const fresh = store.snapshot("second", "screen")?.data ?? "";
		expect(fresh).not.toContain("secret-from-first-shell");
		store.feed("second", new TextEncoder().encode("only-this\r\n"));
		const after = store.snapshot("second", "screen")?.data ?? "";
		expect(after).toContain("only-this");
		expect(after).not.toContain("secret-from-first-shell");
		store.close("second");
	});

	test("open before warm fails loudly rather than silently skipping replay", () => {
		expect(() => new ShellReplayStore().open("shell-1", COLS, ROWS)).toThrow(
			/before warm\(\) resolved/,
		);
	});
});
