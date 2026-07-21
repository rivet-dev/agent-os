// Nightly: requires the non-core Vim command package.
import {
	copyFileSync,
	cpSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import pkg from "@xterm/headless";
import { afterEach, describe, expect, it } from "vitest";
import { __disposeAllSharedSidecarsForTesting } from "../src/agent-os.js";
import type { AgentOs } from "../src/index.js";
import { allowAll } from "../src/runtime-compat.js";

const { Terminal } = pkg;

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "../../..");
// Local-fixture locations, overridable so the suite is not tied to one
// machine's layout (CI skips via the fixture gate below).
// The vim binary comes from the @agentos-software/vim registry package (an
// unbuilt placeholder has no bin/vim -> the suite skips). Overridable for
// one-off fixture builds.
const VIM_PACKAGE_DIR = resolve(
	REPO_ROOT,
	"../agentos/software/vim/dist/package",
);
const VIM_COMMAND_DIR =
	process.env.AGENTOS_VIM_FIXTURE_DIR ?? resolve(VIM_PACKAGE_DIR, "bin");
const VIM_BINARY = resolve(VIM_COMMAND_DIR, "vim");
const SNAP_DIR =
	process.env.AGENTOS_VIM_SNAPSHOT_DIR ??
	"/home/nathan/progress/agent-os/2026-06-30-package-provisioned-files-env/vim-provides-snapshots";

// Mirror packages/shell/src/main.ts: VIMRUNTIME pointed straight at a runtime
// dir bypasses vim's version-name search, so a host 9.0/9.1 runtime sources
// cleanly under the 9.2 binary.
const VIM_RUNTIME_GUEST_DIR = "/usr/local/share/vim/vim92";
const VIM_RUNTIME_CANDIDATES = [
	resolve(VIM_COMMAND_DIR, "vim-runtime"),
	"/usr/share/vim/vim92",
	"/usr/share/vim/vim91",
	"/usr/share/vim/vim90",
	"/usr/local/share/vim/vim92",
];

// Crucially NO `-u NONE`: vim sources `$VIMRUNTIME/defaults.vim` at startup,
// which is the exact path `provides` must satisfy. The other flags isolate the
// runtime as the only variable under test (they do NOT suppress runtime
// loading): `-n` disables the swap file (the VM cannot create vim swap files —
// see ~/.agents/todo, tracked separately), `-i NONE` disables viminfo, and
// `set noesckeys` is the headless-harness ESC-timing aid used by the sibling
// vim-interactive test. So any startup failure here is attributable to the
// runtime/provides, not to swap or ESC timing.
const BARE_VIM_ARGS = ["-n", "-i", "NONE", "--cmd", "set noesckeys"];
const ESC = Uint8Array.of(0x1b);
const TEST_TIMEOUT_MS = 180_000;

function resolveVimRuntimeHostDir(): string {
	const found = VIM_RUNTIME_CANDIDATES.find((dir) =>
		existsSync(resolve(dir, "defaults.vim")),
	);
	if (!found) {
		throw new Error(
			`no vim runtime found (need a dir with defaults.vim among ${VIM_RUNTIME_CANDIDATES.join(", ")})`,
		);
	}
	return found;
}

function assertVimAvailable() {
	if (!existsSync(VIM_BINARY)) {
		throw new Error(`vim wasm fixture not found at ${VIM_BINARY}`);
	}
	const magic = readFileSync(VIM_BINARY).subarray(0, 4);
	expect([...magic]).toEqual([0x00, 0x61, 0x73, 0x6d]);
}

// Materialize the `local-editors` package directory used by the sibling shell
// integration. In the package model the runtime `provides` (VIMRUNTIME env + the
// runtime file overlay) lives in the package's `agentos-package.json`, NOT inline
// on the software descriptor. With `withProvides`, copy the host runtime tree
// into the package under `runtime/` and point `provides.files.source` at that
// package-relative path; without it, ship only the `bin/vim` command so bare vim
// cannot source defaults.vim (the control case).
function materializeLocalEditorsPackage(withProvides: boolean): {
	packagePath: string;
} {
	const packageDir = mkdtempSync(join(tmpdir(), "agentos-local-editors-"));
	const binDir = join(packageDir, "bin");
	mkdirSync(binDir);
	copyFileSync(VIM_BINARY, join(binDir, "vim"));
	writeFileSync(
		join(packageDir, "package.json"),
		JSON.stringify({ name: "local-editors", version: "0.0.0" }),
	);

	let provides:
		| {
				env: Record<string, string>;
				files: Array<{ source: string; target: string }>;
			}
		| undefined;
	if (withProvides) {
		cpSync(resolveVimRuntimeHostDir(), join(packageDir, "runtime"), {
			recursive: true,
		});
		provides = {
			env: {
				VIMRUNTIME: VIM_RUNTIME_GUEST_DIR,
				VIM: "/usr/local/share/vim",
			},
			files: [{ source: "runtime", target: VIM_RUNTIME_GUEST_DIR }],
		};
	}
	writeFileSync(
		join(packageDir, "agentos-package.json"),
		JSON.stringify({
			name: "local-editors",
			version: "1.0.0",
			...(provides ? { provides } : {}),
		}),
	);
	return { packagePath: packageDir };
}

function screen(term: InstanceType<typeof Terminal>): string {
	const buffer = term.buffer.active;
	const lines: string[] = [];
	for (let y = 0; y < term.rows; y++) {
		const line = buffer.getLine(y);
		lines.push((line ? line.translateToString(true) : "").replace(/\s+$/, ""));
	}
	return `${lines.join("\n").replace(/\n+$/, "")}\n`;
}

async function sleep(ms: number) {
	await new Promise((resolve) => setTimeout(resolve, ms));
}

// Requires the vim wasm binary staged locally at `.local-cmds/vim`. CI does not
// build or stage wasm editors, so skip when the fixture is absent rather than
// failing the suite (same policy as brush-interactive).
describe.skipIf(!existsSync(VIM_BINARY))("bare vim runtime via package provides", () => {
	let vm: AgentOs | undefined;

	afterEach(async () => {
		await vm?.dispose().catch(() => {});
		vm = undefined;
	}, 120_000);

	it(
		"provisions the vim runtime + VIMRUNTIME so bare vim starts clean and writes a file",
		async () => {
			assertVimAvailable();
			mkdirSync(SNAP_DIR, { recursive: true });

			const { AgentOs } = await import("../src/index.js");
			vm = await AgentOs.create({
				permissions: allowAll,
				// Note: VIMRUNTIME is intentionally NOT in the shell env below — it
				// must reach vim via `provides.env` -> VM base env. The runtime tree
				// reaches the guest via `provides.files` (overlay lower).
				software: [materializeLocalEditorsPackage(true)],
			});
			await vm.mkdir("/work", { recursive: true });

			const term = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
			let writes = Promise.resolve();
			let snapshotIndex = 0;

			const { shellId } = vm.openShell({
				command: "vim",
				args: BARE_VIM_ARGS,
				cols: 80,
				rows: 24,
				cwd: "/work",
				env: { TERM: "xterm" },
			});
			const offData = vm.onShellData(shellId, (data) => {
				const bytes = Buffer.from(data);
				writes = writes.then(
					() => new Promise<void>((resolve) => term.write(bytes, resolve)),
				);
			});

			const settle = async (ms = 700) => {
				await sleep(ms);
				await writes;
				await sleep(20);
				await writes;
			};
			const waitForScreen = async (
				predicate: (current: string) => boolean,
				label: string,
				timeoutMs = 20_000,
			) => {
				const deadline = Date.now() + timeoutMs;
				let current = screen(term);
				while (Date.now() < deadline) {
					await settle(250);
					current = screen(term);
					if (predicate(current)) {
						return current;
					}
				}
				throw new Error(`timed out waiting for ${label}\n\n${current}`);
			};
			const snap = async (label: string, ms = 700) => {
				await settle(ms);
				const nn = String(snapshotIndex).padStart(2, "0");
				writeFileSync(
					resolve(SNAP_DIR, `${nn}.txt`),
					`## ${nn} - ${label}\n## (bare vim args: ${JSON.stringify(BARE_VIM_ARGS)})\n----- screen 80x24 -----\n${screen(term)}`,
				);
				snapshotIndex++;
				return screen(term);
			};

			// The crux: bare vim found $VIMRUNTIME and sourced defaults.vim cleanly.
			const startup = await waitForScreen(
				(current) =>
					current.includes("VIM - Vi IMproved") &&
					!current.includes("Press ENTER"),
				"vim startup splash (clean, runtime loaded)",
				20_000,
			);
			await snap("startup (bare vim, runtime via provides)", 300);
			expect(startup).toContain("VIM - Vi IMproved");
			expect(startup).not.toContain("Press ENTER");
			expect(startup).not.toContain("E1187");
			expect(startup).not.toContain("defaults.vim");

			const seq: Array<[string | Uint8Array, string, number?]> = [
				[":", "type : (enter command-line)"],
				["e", "e"],
				[" ", "space"],
				["p", "p"],
				[".", "."],
				["t", "t"],
				["x", "x"],
				["t", "t"],
				["\r", "Enter -> run :e p.txt (open new file)"],
				["i", "i (enter INSERT mode)"],
				["p", "p"],
				["r", "r"],
				["o", "o"],
				["v", "v"],
				["i", "i"],
				["d", "d"],
				["e", "e"],
				["s", "s"],
				[" ", "space"],
				["w", "w"],
				["o", "o"],
				["r", "r"],
				["k", "k"],
				["s", "s"],
				[ESC, "ESC (back to NORMAL)", 900],
				[":", "type : (command-line)"],
				["w", "w"],
				["q", "q"],
				["\r", "Enter -> run :wq (write + quit)", 1200],
			];

			const snapshots: string[] = [];
			for (const [key, label, delayMs] of seq) {
				await vm.writeShell(shellId, key);
				snapshots.push(await snap(label, delayMs ?? 650));
			}

			const opened = snapshots[8] ?? "";
			expect(opened).toContain('"p.txt" [New]');
			expect(opened).not.toContain("E1187");

			const insert = snapshots[9] ?? "";
			expect(insert).toContain("-- INSERT --");

			const typed = snapshots[23] ?? "";
			expect(typed).toContain("provides works");

			const written = snapshots.at(-1) ?? "";
			expect(written).toContain('"p.txt"');
			expect(written).toContain("written");
			expect(written).not.toContain("Press ENTER");
			expect(written).not.toContain("E1187");

			await settle(1200);
			const fileContent = Buffer.from(await vm.readFile("/work/p.txt")).toString(
				"utf8",
			);
			writeFileSync(
				resolve(SNAP_DIR, "FILE.txt"),
				`# /work/p.txt after :wq\n${JSON.stringify(fileContent)}\n\n---raw---\n${fileContent}`,
			);
			expect(fileContent).toBe("provides works\n");

			offData();
			void __disposeAllSharedSidecarsForTesting().catch(() => {});
			vm = undefined;
		},
		TEST_TIMEOUT_MS,
	);

	it(
		"control: WITHOUT provides, bare vim fails to source defaults.vim (E1187 / Press ENTER)",
		async () => {
			assertVimAvailable();

			const { AgentOs } = await import("../src/index.js");
			vm = await AgentOs.create({
				permissions: allowAll,
				software: [materializeLocalEditorsPackage(false)],
			});
			await vm.mkdir("/work", { recursive: true });

			const term = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
			let writes = Promise.resolve();
			const { shellId } = vm.openShell({
				command: "vim",
				args: BARE_VIM_ARGS,
				cols: 80,
				rows: 24,
				cwd: "/work",
				env: { TERM: "xterm" },
			});
			const offData = vm.onShellData(shellId, (data) => {
				const bytes = Buffer.from(data);
				writes = writes.then(
					() => new Promise<void>((resolve) => term.write(bytes, resolve)),
				);
			});
			const settle = async (ms = 700) => {
				await sleep(ms);
				await writes;
				await sleep(20);
				await writes;
			};

			// No runtime provisioned -> vim cannot source defaults.vim. Prove the
			// failure surface so the positive test above can't pass for unrelated
			// reasons (provides is load-bearing, not incidental).
			const deadline = Date.now() + 25_000;
			let current = screen(term);
			let failed = false;
			while (Date.now() < deadline) {
				await settle(300);
				current = screen(term);
				if (current.includes("E1187") || current.includes("Press ENTER")) {
					failed = true;
					break;
				}
			}
			writeFileSync(
				resolve(SNAP_DIR, "CONTROL-no-provides.txt"),
				`## control: bare vim WITHOUT provides (expect E1187 / Press ENTER)\n----- screen 80x24 -----\n${current}`,
			);
			expect(failed).toBe(true);

			// Dismiss the prompt so teardown is clean.
			await vm.writeShell(shellId, "\r");
			await vm.writeShell(shellId, ":q!\r");
			await settle(800);

			offData();
			void __disposeAllSharedSidecarsForTesting().catch(() => {});
			vm = undefined;
		},
		TEST_TIMEOUT_MS,
	);
});
