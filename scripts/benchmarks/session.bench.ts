/**
 * Session-creation "VM tax" benchmark (JS layer, llmock, deterministic).
 *
 * Measures the cost of getting a pi agent ready across lanes and reports how much
 * of it is the agentOS VM vs the inherent Node.js pi-SDK work:
 *
 *   lane `vm`        — AgentOs.create() (vmCreate) + openSession({ sessionId, agent: "pi" })
 *   lane `bare-node` — the SAME pi-SDK session construction on host node, no VM
 *                      (sessionCreate = load pi SDK + createAgentSession)
 *
 *   derived.vmTaxMs    = vm.sessionCreate.p50 − bareNode.sessionCreate.p50
 *   derived.vmTaxRatio = vm.sessionCreate.p50 / bareNode.sessionCreate.p50
 *
 * All LLM traffic goes to a local llmock, so vmCreate/sessionCreate are
 * deterministic and gate-able. Prompt latency is intentionally NOT measured here
 * (it's LLM-bound; it belongs in a separate informational real-API suite).
 *
 * Usage:
 *   pnpm exec tsx scripts/benchmarks/session.bench.ts [--iterations=N] [--warmup=N]
 *                 [--lanes=vm,bare-node] [--gate] [--update-baseline]
 *
 *   --gate             exit non-zero if a gated metric regresses vs baseline.json
 *   --update-baseline  overwrite baseline.json with this run (review the diff in a PR)
 */

import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import os from "node:os";
import { dirname, join } from "node:path";
import { performance } from "node:perf_hooks";
import { AgentOs } from "@rivet-dev/agentos-core";
import { LLMock } from "@copilotkit/llmock";

const BASELINE_PATH = join(import.meta.dirname, "baseline.json");
const REPO_ROOT = join(import.meta.dirname, "..", "..");

interface PhaseStats {
	mean: number;
	p50: number;
	p95: number;
	p99: number;
	min: number;
	max: number;
	stddev: number;
}

interface BenchMetadata {
	timestamp: string;
	gitSha: string;
	gitDirty: boolean;
	hardware: {
		cpu: string;
		cores: number;
		ram: string;
		node: string;
		os: string;
		arch: string;
	};
	deps: Record<string, string>;
	llmock: boolean;
	iterations: number;
	warmup: number;
}

interface BenchResult extends BenchMetadata {
	benchmark: string;
	lanes: Record<string, Record<string, PhaseStats>>;
	derived: Record<string, number>;
}

interface GateRule {
	path: string;
	tolerance: number;
	noiseFloor?: number;
	label?: string;
}

function round(n: number, decimals = 2): number {
	const f = 10 ** decimals;
	return Math.round(n * f) / f;
}

function percentile(sorted: number[], p: number): number {
	const idx = Math.ceil((p / 100) * sorted.length) - 1;
	return sorted[Math.max(0, Math.min(sorted.length - 1, idx))];
}

function stats(samples: number[]): PhaseStats {
	const sorted = [...samples].sort((a, b) => a - b);
	const mean = samples.reduce((a, b) => a + b, 0) / samples.length;
	const variance =
		samples.reduce((a, b) => a + (b - mean) ** 2, 0) / samples.length;
	return {
		mean: round(mean),
		p50: round(percentile(sorted, 50)),
		p95: round(percentile(sorted, 95)),
		p99: round(percentile(sorted, 99)),
		min: round(sorted[0]),
		max: round(sorted[sorted.length - 1]),
		stddev: round(Math.sqrt(variance)),
	};
}

function safe<T>(fn: () => T, fallback: T): T {
	try {
		return fn();
	} catch {
		return fallback;
	}
}

function pkgVersion(name: string): string {
	return safe(() => {
		const req = createRequire(join(REPO_ROOT, "package.json"));
		let dir = dirname(req.resolve(name));
		for (let i = 0; i < 8; i++) {
			const candidate = join(dir, "package.json");
			if (existsSync(candidate)) {
				const pkg = JSON.parse(readFileSync(candidate, "utf8"));
				if (pkg.name === name && pkg.version) return pkg.version;
			}
			const parent = dirname(dir);
			if (parent === dir) break;
			dir = parent;
		}
		return "unknown";
	}, "absent");
}

function collectMetadata(opts: {
	iterations: number;
	warmup: number;
	llmock: boolean;
}): BenchMetadata {
	const cpus = os.cpus();
	return {
		timestamp: new Date().toISOString(),
		gitSha: process.env.GITHUB_SHA?.slice(0, 12) ?? "unknown",
		gitDirty: false,
		hardware: {
			cpu: cpus[0]?.model ?? "unknown",
			cores: os.availableParallelism(),
			ram: `${round(os.totalmem() / 1024 ** 3, 1)} GB`,
			node: process.version,
			os: `${os.type()} ${os.release()}`,
			arch: os.arch(),
		},
		deps: {
			"@rivet-dev/agentos-core": pkgVersion("@rivet-dev/agentos-core"),
			"@rivet-dev/agentos-sidecar": pkgVersion("@rivet-dev/agentos-sidecar"),
			"@rivet-dev/agentos-runtime-core": pkgVersion("@rivet-dev/agentos-runtime-core"),
			"@agentos-software/pi": pkgVersion("@agentos-software/pi"),
			"@mariozechner/pi-coding-agent": pkgVersion(
				"@mariozechner/pi-coding-agent",
			),
		},
		iterations: opts.iterations,
		warmup: opts.warmup,
		llmock: opts.llmock,
	};
}

function loadBaseline(): BenchResult | null {
	if (!existsSync(BASELINE_PATH)) return null;
	return JSON.parse(readFileSync(BASELINE_PATH, "utf8")) as BenchResult;
}

function writeBaseline(result: BenchResult): void {
	writeFileSync(BASELINE_PATH, `${JSON.stringify(result, null, 2)}\n`);
}

function resolvePath(result: BenchResult, path: string): number | undefined {
	const parts = path.split(".");
	if (parts[0] === "derived") return result.derived[parts[1]];
	const [lane, metric, field] = parts;
	const s = result.lanes[lane]?.[metric];
	return s ? (s[field as keyof PhaseStats] as number) : undefined;
}

interface GateOutcome {
	path: string;
	label: string;
	baseline: number | undefined;
	current: number | undefined;
	deltaPct: number | undefined;
	tolerance: number;
	regressed: boolean;
}

function evaluateGate(
	current: BenchResult,
	baseline: BenchResult | null,
	rules: GateRule[],
): GateOutcome[] {
	return rules.map((rule) => {
		const cur = resolvePath(current, rule.path);
		const base = baseline ? resolvePath(baseline, rule.path) : undefined;
		const deltaPct =
			base !== undefined && base !== 0 && cur !== undefined
				? round(((cur - base) / base) * 100)
				: undefined;
		const absDelta =
			base !== undefined && cur !== undefined ? cur - base : undefined;
		const regressed =
			deltaPct !== undefined &&
			absDelta !== undefined &&
			deltaPct > rule.tolerance * 100 &&
			absDelta > (rule.noiseFloor ?? 0);
		return {
			path: rule.path,
			label: rule.label ?? rule.path,
			baseline: base,
			current: cur,
			deltaPct,
			tolerance: rule.tolerance,
			regressed,
		};
	});
}

function printDeltaTable(outcomes: GateOutcome[]): void {
	const headers = ["metric", "baseline", "current", "delta%", "budget%", ""];
	const rows = outcomes.map((o) => [
		o.label,
		o.baseline ?? "-",
		o.current ?? "-",
		o.deltaPct === undefined ? "-" : `${o.deltaPct > 0 ? "+" : ""}${o.deltaPct}`,
		`+${round(o.tolerance * 100)}`,
		o.baseline === undefined ? "NEW" : o.regressed ? "REGRESSED" : "ok",
	]);
	const widths = headers.map((h, i) =>
		Math.max(h.length, ...rows.map((r) => String(r[i]).length)),
	);
	const fmt = (row: (string | number)[]) =>
		row.map((c, i) => String(c).padStart(widths[i])).join(" | ");
	console.error("");
	console.error(fmt(headers));
	console.error(widths.map((w) => "-".repeat(w)).join("-+-"));
	for (const row of rows) console.error(fmt(row));
	console.error("");
}

// ── args ─────────────────────────────────────────────────────────────
const argv = process.argv.slice(2);
const arg = (k: string, d: string) =>
	argv.find((a) => a.startsWith(`--${k}=`))?.split("=")[1] ?? d;
const flag = (k: string) => argv.includes(`--${k}`);

const ITERATIONS = Number.parseInt(arg("iterations", "5"), 10);
const WARMUP = Number.parseInt(arg("warmup", "1"), 10);
const LANES = arg("lanes", "vm,bare-node").split(",");
const GATE = flag("gate");
const UPDATE_BASELINE = flag("update-baseline");
const TRACE = flag("trace");
// By default the vm lane reuses one sidecar across sessions (production pattern:
// build the snapshot once, reuse it). Pass --fresh-vm-per-session for the cold
// path (a fresh sidecar per session, so the snapshot cache is never reused).
const FRESH_VM_PER_SESSION = flag("fresh-vm-per-session");

if (flag("help")) {
	console.log(
		"Usage: pnpm exec tsx scripts/benchmarks/session.bench.ts [--iterations=N] [--warmup=N] [--lanes=vm,bare-node] [--gate] [--update-baseline]",
	);
	process.exit(0);
}

// ── gate rules: p50 of deterministic metrics + the hardware-independent ratio ──
const GATE_RULES: GateRule[] = [
	// noiseFloor keeps tiny/fast metrics from flaking on sub-ms jitter.
	{ path: "vm.vmCreate.p50", tolerance: 0.15, noiseFloor: 10, label: "vm.vmCreate p50" },
	{ path: "vm.sessionCreate.p50", tolerance: 0.12, noiseFloor: 40, label: "vm.sessionCreate p50" },
	{ path: "derived.vmTaxRatio", tolerance: 0.15, noiseFloor: 0.1, label: "VM-tax ratio (vm/bare)" },
];

// ── shared llmock ────────────────────────────────────────────────────
let llmock: LLMock | undefined;
let llmockUrl = "";
let llmockPort = 0;
async function ensureLlmock() {
	if (llmock) return { url: llmockUrl, port: llmockPort };
	llmock = new LLMock({ port: 0, logLevel: "silent" });
	llmock.addFixtures([
		{ match: { predicate: () => true }, response: { content: "Hello from llmock" } },
	]);
	llmockUrl = await llmock.start();
	llmockPort = Number(new URL(llmockUrl).port);
	return { url: llmockUrl, port: llmockPort };
}

// ── workload resolution (robust to repo layout) ──────────────────────

async function loadPiSoftware(): Promise<unknown> {
	// Preferred: the in-repo registry build — that is the code under test, and the
	// only build that carries the agent-SDK snapshot (agent.snapshot + the
	// dist/sdk-snapshot.js bundle). The published @agentos-software/pi may predate
	// the snapshot work, so benchmarking against it silently measures the
	// non-snapshot fallback path and hides the optimization entirely.
		const local = join(
			import.meta.dirname,
			"../../../agentos/software/pi/dist/index.js",
		);
	if (existsSync(local)) return (await import(local)).default;
	// Fallback: the published/installed software package. Variable specifier so
	// this typechecks even when the package isn't installed in the dev workspace.
	const piPkg = "@agentos-software/pi";
	try {
		return (await import(piPkg)).default;
	} catch {
		throw new Error(
				"Could not resolve the pi software package (../agentos/software/pi/dist or @agentos-software/pi). Build it first.",
		);
	}
}

const PI_SDK_PKG = "@mariozechner/pi-coding-agent";

/** Locate the installed pi-coding-agent package root on the host, or null. */
function findPiSdkRoot(): string | null {
	const reqs = [
		createRequire(join(import.meta.dirname, "../../package.json")),
			createRequire(
				join(
					import.meta.dirname,
					"../../../agentos/software/pi/package.json",
				),
			),
	];
	for (const req of reqs) {
		for (const base of req.resolve.paths(PI_SDK_PKG) ?? []) {
			const root = join(base, PI_SDK_PKG);
			if (existsSync(join(root, "package.json"))) return root;
		}
		try {
			// pnpm symlink layout: resolve the entry, then walk up to the pkg root.
			let dir = dirname(req.resolve(PI_SDK_PKG));
			for (let i = 0; i < 10; i++) {
				const pj = join(dir, "package.json");
				if (
					existsSync(pj) &&
					JSON.parse(readFileSync(pj, "utf8")).name === PI_SDK_PKG
				) {
					return dir;
				}
				const parent = dirname(dir);
				if (parent === dir) break;
				dir = parent;
			}
		} catch {
			/* not resolvable via this require context */
		}
	}
	return null;
}

/**
 * Resolve the pi SDK package root, or throw (hard error) if it isn't installed —
 * the Node.js-equivalent baseline must actually run, not silently vanish.
 */
function resolvePiSdkRootOrThrow(): string {
	const root = findPiSdkRoot();
	if (!root) {
		throw new Error(
			`bare-node lane: ${PI_SDK_PKG} is not installed/resolvable on the host. ` +
				"Run `pnpm install` (it's a devDependency), or pass --lanes=vm to skip this lane.",
		);
	}
	return root;
}

/**
 * The bare-node session-creation script, run in a FRESH node process per sample
 * so each pays the full cold SDK load (the VM lane reloads the SDK in a fresh V8
 * isolate every session, so this is the apples-to-apples "Node.js equivalent").
	 * Mirrors agentos software/pi/src/adapter.ts `newSession`, with no VM. It times the
 * SDK load + session construction internally and prints `__MS__=<ms>`.
 */
function bareNodeScript(root: string): string {
	return `
import { performance } from "node:perf_hooks";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
const root = ${JSON.stringify(root)};
const phases = [];
const t0 = performance.now();
async function span(name, fn) {
  const s = performance.now();
  try { return await fn(); }
  finally { phases.push({ name, cat: "pi", ph: "X", pid: 1, tid: 1, ts: (s - t0) * 1000, dur: (performance.now() - s) * 1000 }); }
}
const [sdk, settings, resource, session] = await span("loadPiSdkRuntime", () => Promise.all([
  import(join(root, "dist/core/sdk.js")),
  import(join(root, "dist/core/settings-manager.js")),
  import(join(root, "dist/core/resource-loader.js")),
  import(join(root, "dist/core/session-manager.js")),
]));
if (typeof sdk.createAgentSession !== "function") {
  console.error("pi SDK API changed: createAgentSession missing");
  process.exit(2);
}
const cwd = tmpdir();
const agentDir = join(homedir(), ".pi", "agent");
const settingsManager = settings.SettingsManager.create(cwd, agentDir);
const resourceLoader = new resource.DefaultResourceLoader({ cwd, agentDir });
await span("resourceLoader.reload", () => resourceLoader.reload());
await span("createAgentSession", () => sdk.createAgentSession({
  cwd, agentDir,
  sessionManager: session.SessionManager.inMemory(cwd),
  resourceLoader, settingsManager,
  tools: sdk.createCodingTools(cwd, {}),
  customTools: sdk.createCodingTools(cwd, {}),
}));
const total = performance.now() - t0;
console.log("__MS__=" + total);
console.log("__TRACE__=" + JSON.stringify([{ name: "newSession", cat: "pi", ph: "X", pid: 1, tid: 1, ts: 0, dur: total * 1000 }, ...phases]));
process.exit(0);
`;
}

type TraceEvent = {
	name: string;
	ph: string;
	ts: number;
	dur: number;
	pid: number;
	tid: number;
	cat?: string;
};

function runBareNode(
	root: string,
	llmockUrlArg: string,
): Promise<{ ms: number; trace: TraceEvent[] }> {
	return new Promise((res, rej) => {
		const child = spawn(
			process.execPath,
			["--input-type=module", "-e", bareNodeScript(root)],
			{
				env: {
					...process.env,
					ANTHROPIC_BASE_URL: llmockUrlArg,
					ANTHROPIC_API_KEY: "bench-key",
				},
				stdio: ["ignore", "pipe", "pipe"],
			},
		);
		let out = "";
		let err = "";
		child.stdout.on("data", (d) => {
			out += d;
		});
		child.stderr.on("data", (d) => {
			err += d;
		});
		child.on("error", rej);
		child.on("exit", (code) => {
			if (code !== 0) {
				rej(
					new Error(
						`bare-node subprocess exited ${code}: ${(err || out).slice(-400)}`,
					),
				);
				return;
			}
			const m = out.match(/__MS__=([\d.]+)/);
			if (!m) {
				rej(new Error(`bare-node subprocess produced no timing: ${out.slice(-200)}`));
				return;
			}
			const tm = out.match(/__TRACE__=(.+)/);
			res({ ms: Number(m[1]), trace: tm ? JSON.parse(tm[1]) : [] });
		});
	});
}

async function bareNodeLaneSample(
	root: string,
	llmockUrlArg: string,
): Promise<Sample> {
	const { ms } = await runBareNode(root, llmockUrlArg);
	return { sessionCreate: ms };
}

// ── lanes ────────────────────────────────────────────────────────────
type Sample = Record<string, number>;

// VM lane: create ONE VM/sidecar and loop open/delete on it, so the
// process-wide agent-SDK snapshot cache is exercised the way it is in production
// (build-once-per-sidecar, then reuse). A fresh VM per session — the old
// behavior — gives every session a cold cache, which hides the snapshot benefit
// entirely. The WARMUP sessions build/warm the snapshot; the measured sessions
// restore from the cached blob. vmCreate is a single create, reported as one
// sample. To measure the cold per-VM path instead, pass --fresh-vm-per-session.
async function runVmLane(
	software: unknown,
): Promise<Record<string, PhaseStats>> {
	const { port, url } = await ensureLlmock();
	const sessionEnv = {
		ANTHROPIC_API_KEY: "bench-key",
		ANTHROPIC_BASE_URL: url,
	};
	const sessionCreates: number[] = [];
	const vmCreates: number[] = [];

	if (FRESH_VM_PER_SESSION) {
		// Cold path: fresh sidecar each session (snapshot cache never reused).
		for (let i = 0; i < ITERATIONS + WARMUP; i++) {
			const warm = i < WARMUP;
			let t = performance.now();
			const vm = await AgentOs.create({
				software: [software] as never,
				loopbackExemptPorts: [port],
			});
			const vmCreate = performance.now() - t;
			t = performance.now();
			const sessionId = "main";
			await vm.openSession({ sessionId,
				agent: "pi",
				env: sessionEnv,
			});
			const sessionCreate = performance.now() - t;
			await vm.deleteSession({ sessionId });
			await vm.dispose();
			if (warm) continue;
			vmCreates.push(vmCreate);
			sessionCreates.push(sessionCreate);
			console.error(
				`[vm] iter ${i - WARMUP}: vmCreate=${vmCreate.toFixed(0)} sessionCreate=${sessionCreate.toFixed(0)} ms`,
			);
		}
		return { vmCreate: stats(vmCreates), sessionCreate: stats(sessionCreates) };
	}

	// Reused path (default): one sidecar, N sessions — snapshot built once.
	let t = performance.now();
	const vm = await AgentOs.create({
		software: [software] as never,
		loopbackExemptPorts: [port],
	});
	vmCreates.push(performance.now() - t);
	// Await deletion between iterations so adapters do not pile up in the shared
	// sidecar and confound the next openSession measurement.
	try {
		for (let i = 0; i < ITERATIONS + WARMUP; i++) {
			const warm = i < WARMUP;
			t = performance.now();
			const sessionId = `benchmark-${i}`;
			await vm.openSession({ sessionId,
				agent: "pi",
				env: sessionEnv,
			});
			const sessionCreate = performance.now() - t;
			await vm.deleteSession({ sessionId });
			if (warm) continue;
			sessionCreates.push(sessionCreate);
			console.error(
				`[vm] iter ${i - WARMUP}: sessionCreate=${sessionCreate.toFixed(0)} ms (reused sidecar)`,
			);
		}
	} finally {
		await vm.dispose();
	}
	return { vmCreate: stats(vmCreates), sessionCreate: stats(sessionCreates) };
}

async function runLane(
	name: string,
	sampleFn: () => Promise<Sample>,
): Promise<Record<string, PhaseStats>> {
	const collected: Record<string, number[]> = {};
	for (let i = 0; i < ITERATIONS + WARMUP; i++) {
		const warm = i < WARMUP;
		const sample = await sampleFn();
		if (warm) continue;
		for (const [k, v] of Object.entries(sample)) {
			(collected[k] ??= []).push(v);
		}
		const line = Object.entries(sample)
			.map(([k, v]) => `${k}=${v.toFixed(0)}`)
			.join(" ");
		console.error(`[${name}] iter ${i - WARMUP}: ${line} ms`);
	}
	const out: Record<string, PhaseStats> = {};
	for (const [k, v] of Object.entries(collected)) out[k] = stats(v);
	return out;
}

// ── main ─────────────────────────────────────────────────────────────
// ── trace mode ───────────────────────────────────────────────────────
const RESULTS_DIR = join(import.meta.dirname, "results");

/** One VM openSession with adapter phase tracing enabled; returns the spans. */
async function vmLaneTrace(software: unknown): Promise<TraceEvent[]> {
	const { port, url } = await ensureLlmock();
	const vm = await AgentOs.create({
		software: [software] as never,
		loopbackExemptPorts: [port],
	});
	const tracePath = "/home/agentos/pi-trace.json";
	const sessionId = "main";
	await vm.openSession({ sessionId,
		agent: "pi",
		env: {
			ANTHROPIC_API_KEY: "bench-key",
			ANTHROPIC_BASE_URL: url,
			PI_TRACE_FILE: tracePath,
		},
	});
	let spans: TraceEvent[] = [];
	try {
		spans = JSON.parse(new TextDecoder().decode(await vm.readFile(tracePath)));
	} catch (e) {
		console.error(`  (no adapter trace: ${(e as Error).message})`);
	}
	await vm.deleteSession({ sessionId });
	await vm.dispose();
	return spans;
}

function phaseMap(spans: TraceEvent[]): Map<string, number> {
	const m = new Map<string, number>();
	for (const s of spans) m.set(s.name, Math.round(s.dur / 1000)); // µs -> ms
	return m;
}

async function traceMode() {
	const out: Record<string, TraceEvent[]> = {};

	console.error("=== trace: vm lane (adapter newSession phases) ===");
	out.vm = await vmLaneTrace(await loadPiSoftware());

	console.error("=== trace: bare-node lane (host pi-SDK phases) ===");
	const { trace } = await runBareNode(
		resolvePiSdkRootOrThrow(),
		(await ensureLlmock()).url,
	);
	out["bare-node"] = trace;

	// Write Chrome-trace / Perfetto JSON per lane (openable at ui.perfetto.dev),
	// and a merged trace with the lanes on separate tracks for side-by-side view.
	const tag = (evs: TraceEvent[], tid: number, prefix: string) =>
		evs.map((e) => ({ ...e, tid, name: `${prefix}${e.name}` }));
	writeFileSync(
		join(RESULTS_DIR, "trace-vm.json"),
		JSON.stringify({ traceEvents: out.vm }, null, 2),
	);
	writeFileSync(
		join(RESULTS_DIR, "trace-bare-node.json"),
		JSON.stringify({ traceEvents: out["bare-node"] }, null, 2),
	);
	writeFileSync(
		join(RESULTS_DIR, "trace-merged.json"),
		JSON.stringify(
			{
				traceEvents: [
					...tag(out.vm, 1, "vm:"),
					...tag(out["bare-node"], 2, "bare:"),
				],
			},
			null,
			2,
		),
	);

	// Per-phase comparison table (the "difference with nodejs perf").
	const vmP = phaseMap(out.vm);
	const bareP = phaseMap(out["bare-node"]);
	const names = [...new Set([...vmP.keys(), ...bareP.keys()])].filter(
		(n) => n !== "newSession",
	);
	const headers = ["phase", "vm (ms)", "bare-node (ms)", "VM tax (ms)", "×"];
	const rows = names.map((n) => {
		const v = vmP.get(n);
		const b = bareP.get(n);
		const tax = v !== undefined && b !== undefined ? v - b : undefined;
		const x = v !== undefined && b ? round(v / b, 1) : undefined;
		return [n, v ?? "—", b ?? "—", tax ?? "—", x ?? "—"];
	});
	rows.push([
		"TOTAL (newSession)",
		vmP.get("newSession") ?? "—",
		bareP.get("newSession") ?? "—",
		(vmP.get("newSession") ?? 0) - (bareP.get("newSession") ?? 0) || "—",
		bareP.get("newSession")
			? round((vmP.get("newSession") ?? 0) / (bareP.get("newSession") ?? 1), 1)
			: "—",
	]);
	const widths = headers.map((h, i) =>
		Math.max(h.length, ...rows.map((r) => String(r[i]).length)),
	);
	const fmt = (r: (string | number)[]) =>
		r.map((c, i) => String(c).padStart(widths[i])).join(" | ");
	console.error("");
	console.error(fmt(headers));
	console.error(widths.map((w) => "-".repeat(w)).join("-+-"));
	for (const r of rows) console.error(fmt(r));
	console.error(
		`\nTraces written to ${RESULTS_DIR}/trace-{vm,bare-node,merged}.json — open at https://ui.perfetto.dev`,
	);
	if (llmock) await llmock.stop();
}

async function main() {
	if (TRACE) {
		await traceMode();
		return;
	}
	const lanes: BenchResult["lanes"] = {};

	if (LANES.includes("vm")) {
		const software = await loadPiSoftware();
		console.error(
			`\n=== lane: vm (AgentOs + openSession${FRESH_VM_PER_SESSION ? ", fresh sidecar/session" : ", reused sidecar"}) ===`,
		);
		lanes.vm = await runVmLane(software);
	}

	if (LANES.includes("bare-node")) {
		console.error("\n=== lane: bare-node (host pi-SDK, fresh process, no VM) ===");
		// Hard error if the SDK isn't installed — the Node.js-equivalent baseline
		// must actually run, not silently vanish from the comparison.
		const root = resolvePiSdkRootOrThrow();
		const { url } = await ensureLlmock();
		lanes["bare-node"] = await runLane("bare-node", () =>
			bareNodeLaneSample(root, url),
		);
	}

	// derived VM tax
	const derived: Record<string, number> = {};
	const vmSess = lanes.vm?.sessionCreate?.p50;
	const bareSess = lanes["bare-node"]?.sessionCreate?.p50;
	if (vmSess !== undefined && bareSess !== undefined && bareSess > 0) {
		derived.vmTaxMs = Math.round(vmSess - bareSess);
		derived.vmTaxRatio = Math.round((vmSess / bareSess) * 100) / 100;
	}

	const result: BenchResult = {
		benchmark: "session",
		...collectMetadata({ iterations: ITERATIONS, warmup: WARMUP, llmock: true }),
		lanes,
		derived,
	};

	// emit result JSON to stdout (run-benchmarks.sh redirects to results/)
	console.log(JSON.stringify(result, null, 2));

	// delta vs baseline
	const baseline = loadBaseline();
	const outcomes = evaluateGate(result, baseline, GATE_RULES);
	console.error("\n── delta vs baseline ──");
	if (!baseline) console.error("(no baseline.json yet — run with --update-baseline to create one)");
	printDeltaTable(outcomes);

	if (UPDATE_BASELINE) {
		writeBaseline(result);
		console.error(`baseline.json updated (${result.gitSha}${result.gitDirty ? "-dirty" : ""}).`);
	}

	if (llmock) await llmock.stop();

	const regressions = outcomes.filter((o) => o.regressed);
	if (GATE && baseline && regressions.length > 0) {
		console.error(`\n❌ ${regressions.length} metric(s) regressed beyond budget.`);
		process.exit(1);
	}
	console.error(GATE ? "\n✓ within budget." : "\n(run with --gate to enforce thresholds)");
}

main().catch((e) => {
	console.error(e);
	process.exit(1);
});
