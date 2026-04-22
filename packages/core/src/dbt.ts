/**
 * Home of everything dbt-specific in the core SDK: types, helper
 * scripts, parsers, and the `AgentOsDbt` namespace.
 *
 * The module is deliberately self-contained — `agent-os.ts` imports the
 * public pieces back out and re-exports them from the package entry for
 * backward compat, but the SDK's dbt concerns live here.
 */

import type { AgentOs } from "./agent-os.js";

// ────────────────────────────────────────────────────────────────────
// Public types
// ────────────────────────────────────────────────────────────────────

/**
 * Snapshot of tripwire counters set by the dbt bootstrap monkey-patches.
 * Every counter is monotonically non-decreasing for the lifetime of the
 * Pyodide worker. Use these to confirm the sync shims actually fired — if
 * a counter stays 0 across a `dbt.run` call, the corresponding patch did
 * not intercept the call it was supposed to intercept.
 */
export interface DbtTripwireSnapshot {
	thread_pool_executor_submit: number;
	dbt_thread_pool_apply_async: number;
	dbt_thread_pool_init: number;
	multiprocessing_get_context: number;
	multiprocessing_dummy_start: number;
	workers_alive: number;
	last_updated: string;
}

/** Options for `aos.dbt.run`. */
export interface RunDbtOptions {
	/**
	 * Working directory the dbt process runs in. Defaults to the project
	 * root auto-mount (`/root/dbt-projects`). For most real projects you
	 * want this to point at a specific project subdirectory.
	 */
	cwd?: string;
	/**
	 * Additional environment variables merged on top of the base env and
	 * DBT_ENV defaults. User values win — this does not override keys you
	 * set here.
	 */
	env?: Record<string, string>;
	/** Called whenever the dbt process emits stdout. */
	onStdout?: (chunk: Uint8Array) => void;
	/** Called whenever the dbt process emits stderr. */
	onStderr?: (chunk: Uint8Array) => void;
}

/**
 * Structured outcome of a dbt CLI invocation.
 *
 * `success` reflects `dbtRunner().invoke(...).success`, not the process
 * exit code — Pyodide's webloop wraps `sys.exit` unreliably, so the
 * helper script avoids raising SystemExit and instead communicates
 * success via a trailing stdout sentinel.
 */
export interface DbtRunResult {
	/** dbtRunner's own success flag. */
	success: boolean;
	/** Exit code of the host Python process. Usually 0 even on dbt failure. */
	exitCode: number;
	/** Full stdout including the sentinel line (stripped from the tail). */
	stdout: string;
	stderr: string;
	/** Python repr of any exception dbtRunner surfaced, else null. */
	exception: string | null;
	/** Tripwire snapshot captured after the run completed. */
	tripwire: DbtTripwireSnapshot | null;
	/**
	 * Aggregate counts parsed from `target/run_results.json` when the
	 * command wrote one (build/run/test/seed/snapshot). `undefined` for
	 * commands that don't produce run_results.json (debug/parse/deps) or
	 * when `options.cwd` wasn't supplied so there was no project root to
	 * probe.
	 */
	stats?: DbtRunStats;
}

/** Aggregate counters derived from dbt's `target/run_results.json`. */
export interface DbtRunStats {
	modelsRun: number;
	modelsPassed: number;
	modelsFailed: number;
	testsRun: number;
	testsPassed: number;
	testsFailed: number;
	/** Sum of per-result `execution_time`, in milliseconds. */
	totalElapsedMs: number;
}

/**
 * Options accepted by the dbt subcommand shortcuts (`build`, `test`,
 * `compile`, `seed`, `deps`, `debug`, `parse`). Extends `RunDbtOptions`
 * with typed versions of the most common CLI flags so callers don't
 * have to splice `--select` / `--exclude` / `--target` / `--vars` into
 * the args array themselves.
 */
export interface DbtSubOptions extends RunDbtOptions {
	/** Node selectors forwarded as `--select a b c`. */
	select?: string[];
	/** Node selectors forwarded as `--exclude a b c`. */
	exclude?: string[];
	/** Target name forwarded as `--target <name>`. */
	target?: string;
	/** Profile name forwarded as `--profile <name>`. */
	profile?: string;
	/** Vars forwarded as `--vars '<json>'`. JSON is a valid YAML subset for flat maps. */
	vars?: Record<string, unknown>;
}

/** Parsed shape of `target/manifest.json`. */
export interface DbtManifest {
	/** Every manifest node (models, tests, seeds, snapshots, sources, macros…). */
	nodes: DbtManifestNode[];
	/** `manifest.metadata` block (dbt version, generated_at, adapter, etc.). */
	metadata: Record<string, unknown>;
}

/** A single node entry out of the flattened manifest. */
export interface DbtManifestNode {
	uniqueId: string;
	name: string;
	/** dbt's `resource_type` — "model" | "test" | "seed" | "snapshot" | "source" | … */
	resourceType: string;
	path: string;
	description?: string;
	dependsOn: { nodes: string[] };
	meta?: Record<string, unknown>;
}

/** Parsed shape of `target/run_results.json`. */
export interface DbtRunResults {
	args: Record<string, unknown>;
	results: DbtRunResultRow[];
	/** Total elapsed time of the dbt run in seconds (dbt's own metric). */
	elapsedTime: number;
	generatedAt: string;
}

/** A single row of `run_results.json` normalized to camelCase. */
export interface DbtRunResultRow {
	uniqueId: string;
	/** "success" | "error" | "fail" | "pass" | "skipped" | "warn" | "runtime error" */
	status: string;
	/** Per-row execution_time in seconds. */
	executionTime?: number;
	message?: string;
	failures?: number;
}

/** Parsed shape of `target/catalog.json` (produced by `dbt docs generate`). */
export interface DbtCatalog {
	nodes: Record<string, DbtCatalogEntry>;
	sources?: Record<string, DbtCatalogEntry>;
	errors?: Record<string, string>;
}

/** A single table entry in the catalog. */
export interface DbtCatalogEntry {
	metadata: { schema: string; name: string; type?: string };
	columns: Record<
		string,
		{ type: string; index: number; comment?: string | null }
	>;
}

// ────────────────────────────────────────────────────────────────────
// Path + sentinel constants
// ────────────────────────────────────────────────────────────────────

/**
 * SDK scratch directory. Placed inside the auto-mounted profiles dir so
 * the NODEFS bridge makes writes visible to both Python (`open()` from
 * inside Pyodide) and the kernel VFS (`aos.readFile()` from the host /
 * actor side). `/tmp` cannot be used for this — it's Pyodide's MEMFS
 * and is NOT bridged to the kernel VFS, so any file Python writes under
 * `/tmp` is invisible to `aos.readFile()`.
 *
 * Exported so callers that stage their own auxiliary files can follow
 * the same convention.
 */
export const AGENT_OS_SCRATCH_DIR = "/root/.dbt/.aos";

/**
 * Where the dbt helper persists its structured result. Lives inside
 * `AGENT_OS_SCRATCH_DIR` so it's visible on both sides of the NODEFS
 * bridge. Internal: callers should use `aos.dbt.run` which parses the
 * result for them.
 */
const RUN_DBT_RESULT_PATH = `${AGENT_OS_SCRATCH_DIR}/run_dbt_result.json`;

/** Path where the helper is staged inside the VM. */
const RUN_DBT_HELPER_PATH = "/tmp/_agent_os_run_dbt.py";

/**
 * Sentinel that delimits the structured tail of the dbt helper script
 * output. The helper prints `__AGENT_OS_DBT_RESULT_JSON__{...}__END__` as
 * its last line so the host can parse a structured result without
 * competing with dbt's own stdout. Module-local so the helper script
 * and the parser can't drift.
 */
const DBT_RESULT_SENTINEL_BEGIN = "__AGENT_OS_DBT_RESULT_JSON__";
const DBT_RESULT_SENTINEL_END = "__END__";

// ────────────────────────────────────────────────────────────────────
// Helper scripts (Python)
// ────────────────────────────────────────────────────────────────────

/**
 * Python helper that `aos.dbt.run` writes to `RUN_DBT_HELPER_PATH` and
 * invokes via `python3`. Receives dbt's own CLI args starting from
 * argv[1]. Prints dbt's normal output plus a trailing structured JSON
 * line delimited by the sentinels above. Never calls sys.exit so
 * Pyodide's webloop doesn't mangle the exit path.
 */
const RUN_DBT_HELPER_PY = `# agent-os dbt.run helper — auto-installed; do not edit.
import json as _aos_json
import sys as _aos_sys
import traceback as _aos_traceback


def _aos_tripwire_snapshot():
    mod = _aos_sys.modules.get("_agent_os_dbt_tripwire")
    if mod is None:
        return None
    return {
        "thread_pool_executor_submit": int(getattr(mod, "thread_pool_executor_submit", 0)),
        "dbt_thread_pool_apply_async": int(getattr(mod, "dbt_thread_pool_apply_async", 0)),
        "dbt_thread_pool_init": int(getattr(mod, "dbt_thread_pool_init", 0)),
        "multiprocessing_get_context": int(getattr(mod, "multiprocessing_get_context", 0)),
        "multiprocessing_dummy_start": int(getattr(mod, "multiprocessing_dummy_start", 0)),
        "workers_alive": int(getattr(mod, "workers_alive", 0)),
        "last_updated": getattr(mod, "last_updated", "") or "",
    }


_aos_success = False
_aos_exception = None
try:
    from dbt.cli.main import dbtRunner as _aos_dbtRunner
    _aos_res = _aos_dbtRunner().invoke(list(_aos_sys.argv[1:]))
    _aos_success = bool(_aos_res.success)
    if _aos_res.exception is not None:
        _aos_exception = repr(_aos_res.exception)
except BaseException as _aos_err:
    _aos_traceback.print_exc(file=_aos_sys.stderr)
    _aos_exception = repr(_aos_err)

_aos_payload = {
    "success": _aos_success,
    "exception": _aos_exception,
    "tripwire": _aos_tripwire_snapshot(),
}
# Dual-emit the structured result so both paths work:
#   1. stdout sentinel — for aos.dbt.run's in-process stream hooks.
#   2. file at RUN_DBT_RESULT_PATH — for RPC callers that can't stream.
# File write is best-effort: if the scratch dir's parent isn't
# NODEFS-bridged (no dbt auto-mount), the write lands in Pyodide MEMFS
# and callers on the kernel VFS side won't see it. That's fine — they'd
# fall back to parsing the stdout sentinel.
try:
    import os as _aos_os
    _aos_os.makedirs("${AGENT_OS_SCRATCH_DIR}", exist_ok=True)
    with open("${RUN_DBT_RESULT_PATH}", "w") as _aos_out:
        _aos_json.dump(_aos_payload, _aos_out)
except Exception:
    pass
print("${DBT_RESULT_SENTINEL_BEGIN}" + _aos_json.dumps(_aos_payload) + "${DBT_RESULT_SENTINEL_END}", flush=True)
`;

/**
 * Python `-c` probe that reads the `_agent_os_dbt_tripwire` module and
 * prints either "NULL" (module not loaded) or a single-line JSON
 * snapshot. Internal: callers should use `aos.dbt.tripwire()` which
 * invokes + parses this for them.
 */
const DBT_TRIPWIRE_PROBE_PY = `import sys, json
mod = sys.modules.get("_agent_os_dbt_tripwire")
if mod is None:
    print("NULL")
else:
    print(json.dumps({
        "thread_pool_executor_submit": int(getattr(mod, "thread_pool_executor_submit", 0)),
        "dbt_thread_pool_apply_async": int(getattr(mod, "dbt_thread_pool_apply_async", 0)),
        "dbt_thread_pool_init": int(getattr(mod, "dbt_thread_pool_init", 0)),
        "multiprocessing_get_context": int(getattr(mod, "multiprocessing_get_context", 0)),
        "multiprocessing_dummy_start": int(getattr(mod, "multiprocessing_dummy_start", 0)),
        "workers_alive": int(getattr(mod, "workers_alive", 0)),
        "last_updated": getattr(mod, "last_updated", "") or "",
    }))
`;

// ────────────────────────────────────────────────────────────────────
// Parsers + filters
// ────────────────────────────────────────────────────────────────────

/**
 * Parse the single-line output of `DBT_TRIPWIRE_PROBE_PY`. Returns null
 * when the tripwire module is absent (i.e. the VM wasn't booted with
 * `python.dbt: true`) or the output isn't valid JSON.
 */
function parseDbtTripwireProbe(
	output: string,
): DbtTripwireSnapshot | null {
	const trimmed = output.trim();
	if (!trimmed || trimmed === "NULL") return null;
	try {
		return JSON.parse(trimmed) as DbtTripwireSnapshot;
	} catch {
		return null;
	}
}

/**
 * Best-effort parser for the sentinel-delimited result line emitted by
 * `RUN_DBT_HELPER_PY`. Module-local — `aos.dbt.run` is the public entry
 * point that stages the helper, captures stdout, and invokes the
 * parser for you.
 */
function parseDbtResultSentinel(stdout: string): {
	success: boolean;
	exception: string | null;
	tripwire: DbtTripwireSnapshot | null;
	trimmedStdout: string;
} | null {
	const begin = stdout.lastIndexOf(DBT_RESULT_SENTINEL_BEGIN);
	if (begin === -1) return null;
	const payloadStart = begin + DBT_RESULT_SENTINEL_BEGIN.length;
	const endAt = stdout.indexOf(DBT_RESULT_SENTINEL_END, payloadStart);
	if (endAt === -1) return null;
	const raw = stdout.slice(payloadStart, endAt);
	try {
		const parsed = JSON.parse(raw) as {
			success: boolean;
			exception: string | null;
			tripwire: DbtTripwireSnapshot | null;
		};
		// Drop the sentinel line (and any trailing newline) so callers see
		// just dbt's own output.
		const before = stdout.slice(0, begin);
		const trimmedStdout = before.endsWith("\n")
			? before.slice(0, -1)
			: before;
		return {
			success: parsed.success,
			exception: parsed.exception ?? null,
			tripwire: parsed.tripwire ?? null,
			trimmedStdout,
		};
	} catch {
		return null;
	}
}

/**
 * Streaming filter that strips the dbt.run result sentinel from chunks
 * being forwarded to user `onStdout` hooks. The sentinel is an
 * implementation detail — users piping dbt output to their console
 * should never see it. Handles the case where the sentinel is split
 * across multiple chunks by buffering a tail up to `sentinel.length - 1`
 * bytes.
 */
function createDbtStreamFilter(
	forward: ((chunk: Uint8Array) => void) | undefined,
): (chunk: Uint8Array) => void {
	if (!forward) return () => {};
	const beginBytes = new TextEncoder().encode(DBT_RESULT_SENTINEL_BEGIN);
	const minHold = beginBytes.length;
	let buffered = new Uint8Array(0);
	let sentinelSeen = false;
	return (chunk: Uint8Array) => {
		if (sentinelSeen) return;
		const combined = new Uint8Array(buffered.length + chunk.length);
		combined.set(buffered, 0);
		combined.set(chunk, buffered.length);
		const sentinelIdx = findByteSequence(combined, beginBytes);
		if (sentinelIdx !== -1) {
			sentinelSeen = true;
			// Strip a single preceding newline so console output doesn't
			// end with an empty line where the sentinel used to be.
			let end = sentinelIdx;
			if (end > 0 && combined[end - 1] === 0x0a) end -= 1;
			if (end > 0) forward(combined.slice(0, end));
			buffered = new Uint8Array(0);
			return;
		}
		if (combined.length <= minHold - 1) {
			buffered = combined;
			return;
		}
		const safeLen = combined.length - (minHold - 1);
		forward(combined.slice(0, safeLen));
		buffered = combined.slice(safeLen);
	};
}

function findByteSequence(haystack: Uint8Array, needle: Uint8Array): number {
	if (needle.length === 0) return 0;
	outer: for (let i = 0; i <= haystack.length - needle.length; i++) {
		for (let j = 0; j < needle.length; j++) {
			if (haystack[i + j] !== needle[j]) continue outer;
		}
		return i;
	}
	return -1;
}

/**
 * Translate `DbtSubOptions` into the CLI flag array the dbt helper
 * expects. Empty arrays and missing fields produce nothing — the dbt
 * CLI rejects bare `--select` with no values.
 */
function subOptionArgs(opts: DbtSubOptions | undefined): string[] {
	if (!opts) return [];
	const out: string[] = [];
	if (opts.select && opts.select.length > 0) {
		out.push("--select", ...opts.select);
	}
	if (opts.exclude && opts.exclude.length > 0) {
		out.push("--exclude", ...opts.exclude);
	}
	if (opts.target) out.push("--target", opts.target);
	if (opts.profile) out.push("--profile", opts.profile);
	if (opts.vars && Object.keys(opts.vars).length > 0) {
		out.push("--vars", JSON.stringify(opts.vars));
	}
	return out;
}

/**
 * Fold `DbtRunResults` into the flat counters shape surfaced on
 * `DbtRunResult.stats`. `skipped` rows don't count against pass/fail —
 * they count only toward the total rows-seen bucket.
 */
function aggregateRunStats(results: DbtRunResults): DbtRunStats {
	let modelsRun = 0;
	let modelsPassed = 0;
	let modelsFailed = 0;
	let testsRun = 0;
	let testsPassed = 0;
	let testsFailed = 0;
	let totalElapsedSec = 0;
	for (const row of results.results) {
		totalElapsedSec += row.executionTime ?? 0;
		const kind = row.uniqueId.split(".")[0];
		const ok = row.status === "success" || row.status === "pass";
		if (kind === "model") {
			modelsRun++;
			if (ok) modelsPassed++;
			else if (row.status !== "skipped") modelsFailed++;
		} else if (kind === "test") {
			testsRun++;
			if (ok) testsPassed++;
			else if (row.status !== "skipped") testsFailed++;
		}
	}
	return {
		modelsRun,
		modelsPassed,
		modelsFailed,
		testsRun,
		testsPassed,
		testsFailed,
		totalElapsedMs: Math.round(totalElapsedSec * 1000),
	};
}

// ────────────────────────────────────────────────────────────────────
// AgentOsDbt — namespace exposed as `aos.dbt`
// ────────────────────────────────────────────────────────────────────

/**
 * dbt operations namespace. Accessed as `aos.dbt` on an `AgentOs`
 * instance. All methods assume the VM was booted with
 * `python: { dbt: true }`; otherwise the Pyodide runtime won't have
 * dbt-core / DuckDB available and calls will fail at the Python
 * `import dbt.cli.main` line.
 */
export class AgentOsDbt {
	constructor(private readonly aos: AgentOs) {}

	/**
	 * Run dbt inside the VM via the canonical helper script. Captures
	 * stdout/stderr, parses the sentinel-delimited structured result,
	 * and returns a shaped `DbtRunResult`.
	 *
	 * Never throws on dbt failures — inspect `result.success` and
	 * `result.exception`. Only throws if the spawn itself fails (e.g.
	 * python3 not on PATH).
	 *
	 * @example
	 * await aos.writeFiles([
	 *   { path: "/root/dbt-projects/demo/dbt_project.yml", content: PROJECT_YML },
	 *   { path: "/root/dbt-projects/demo/models/example.sql", content: MODEL_SQL },
	 *   { path: "/root/.dbt/profiles.yml", content: PROFILES_YML },
	 * ]);
	 * const r = await aos.dbt.run(["run", "--threads", "1"], {
	 *   cwd: "/root/dbt-projects/demo",
	 * });
	 * if (!r.success) throw new Error(r.exception ?? "dbt failed");
	 */
	async run(
		args: string[],
		options?: RunDbtOptions,
	): Promise<DbtRunResult> {
		// Stage the helper at a stable path; idempotent because the
		// contents are constant. Writing every call keeps the path valid
		// even if something else in the VM overwrote /tmp.
		await this.aos.writeFile(RUN_DBT_HELPER_PATH, RUN_DBT_HELPER_PY);

		let stdout = "";
		let stderr = "";
		const stdoutDecoder = new TextDecoder();
		const stderrDecoder = new TextDecoder();

		// User-facing stdout hook never sees the sentinel: it's an
		// implementation detail we strip before forwarding.
		const forwardToUser = createDbtStreamFilter(options?.onStdout);

		const { pid } = this.aos.spawn(
			"python3",
			[RUN_DBT_HELPER_PATH, ...args],
			{
				cwd: options?.cwd,
				env: options?.env,
				onStdout: (chunk) => {
					stdout += stdoutDecoder.decode(chunk, { stream: true });
					forwardToUser(chunk);
				},
				onStderr: (chunk) => {
					stderr += stderrDecoder.decode(chunk, { stream: true });
					options?.onStderr?.(chunk);
				},
			},
		);
		const exitCode = await this.aos.waitProcess(pid);
		// Flush any buffered multibyte data from each streaming decoder.
		stdout += stdoutDecoder.decode();
		stderr += stderrDecoder.decode();

		const parsed = parseDbtResultSentinel(stdout);
		const base: DbtRunResult = parsed
			? {
					success: parsed.success,
					exitCode,
					stdout: parsed.trimmedStdout,
					stderr,
					exception: parsed.exception,
					tripwire: parsed.tripwire,
				}
			: // Helper never printed the sentinel — likely crashed before
				// reaching the final line. Return a shaped failure so callers
				// get stdout/stderr without having to special-case missing
				// structured data.
				{
					success: false,
					exitCode,
					stdout,
					stderr,
					exception: null,
					tripwire: null,
				};

		// Opportunistic stats: if the caller gave us a project root, read
		// dbt's run_results.json (if present) and fold its rows into flat
		// counters. Subcommands that don't produce run_results.json
		// (debug/parse/deps) will simply leave `stats` undefined.
		if (options?.cwd) {
			const rr = await this.readRunResults(options.cwd);
			if (rr) base.stats = aggregateRunStats(rr);
		}

		return base;
	}

	/**
	 * Run `dbt build` — the pipeline shortcut that runs models, tests,
	 * seeds, and snapshots in dependency order. Equivalent to
	 * `run(["build", …flags(options)], options)`.
	 */
	async build(options?: DbtSubOptions): Promise<DbtRunResult> {
		return this.run(["build", ...subOptionArgs(options)], options);
	}

	/** Run `dbt test`. */
	async test(options?: DbtSubOptions): Promise<DbtRunResult> {
		return this.run(["test", ...subOptionArgs(options)], options);
	}

	/** Run `dbt compile`. */
	async compile(options?: DbtSubOptions): Promise<DbtRunResult> {
		return this.run(["compile", ...subOptionArgs(options)], options);
	}

	/** Run `dbt seed`. */
	async seed(options?: DbtSubOptions): Promise<DbtRunResult> {
		return this.run(["seed", ...subOptionArgs(options)], options);
	}

	/** Run `dbt deps`. No artifact output; `stats` will be undefined. */
	async deps(options?: DbtSubOptions): Promise<DbtRunResult> {
		return this.run(["deps", ...subOptionArgs(options)], options);
	}

	/**
	 * Run `dbt debug`. Does not produce run_results.json, so `stats`
	 * will always be undefined on the returned result.
	 */
	async debug(options?: DbtSubOptions): Promise<DbtRunResult> {
		return this.run(["debug", ...subOptionArgs(options)], options);
	}

	/**
	 * Run `dbt parse` — resolves references and emits
	 * `target/manifest.json` without executing models.
	 */
	async parse(options?: DbtSubOptions): Promise<DbtRunResult> {
		return this.run(["parse", ...subOptionArgs(options)], options);
	}

	/**
	 * Read and parse `target/manifest.json` from the given project root.
	 * Returns `null` if the file doesn't exist or isn't valid JSON —
	 * useful for probing "has this project been parsed yet?" without
	 * having to stat the file yourself.
	 */
	async readManifest(projectDir: string): Promise<DbtManifest | null> {
		const raw = await this._readTargetJson(projectDir, "manifest.json");
		if (!raw || typeof raw !== "object") return null;
		const rawNodes = (raw as Record<string, unknown>).nodes;
		const nodes: DbtManifestNode[] = [];
		if (rawNodes && typeof rawNodes === "object") {
			for (const [uniqueId, value] of Object.entries(
				rawNodes as Record<string, unknown>,
			)) {
				const node = value as Record<string, unknown>;
				const dependsOn = (node.depends_on as Record<string, unknown>) ?? {};
				nodes.push({
					uniqueId,
					name: String(node.name ?? ""),
					resourceType: String(node.resource_type ?? ""),
					path: String(node.path ?? ""),
					description: node.description as string | undefined,
					dependsOn: {
						nodes: Array.isArray(dependsOn.nodes)
							? (dependsOn.nodes as string[])
							: [],
					},
					meta: node.meta as Record<string, unknown> | undefined,
				});
			}
		}
		const metadata =
			((raw as Record<string, unknown>).metadata as Record<string, unknown>) ??
			{};
		return { nodes, metadata };
	}

	/**
	 * Read and parse `target/run_results.json`. Returns `null` if the
	 * file is missing or invalid. Also used internally to populate
	 * `DbtRunResult.stats` after a successful `run`.
	 */
	async readRunResults(projectDir: string): Promise<DbtRunResults | null> {
		const raw = await this._readTargetJson(projectDir, "run_results.json");
		if (!raw || typeof raw !== "object") return null;
		const rawResults = (raw as Record<string, unknown>).results;
		const results: DbtRunResultRow[] = Array.isArray(rawResults)
			? rawResults.map((entry) => {
					const row = entry as Record<string, unknown>;
					return {
						uniqueId: String(row.unique_id ?? ""),
						status: String(row.status ?? ""),
						executionTime:
							typeof row.execution_time === "number"
								? row.execution_time
								: undefined,
						message:
							typeof row.message === "string" ? row.message : undefined,
						failures:
							typeof row.failures === "number" ? row.failures : undefined,
					};
				})
			: [];
		return {
			args: ((raw as Record<string, unknown>).args as Record<
				string,
				unknown
			>) ?? {},
			results,
			elapsedTime: Number((raw as Record<string, unknown>).elapsed_time ?? 0),
			generatedAt: String((raw as Record<string, unknown>).generated_at ?? ""),
		};
	}

	/**
	 * Read and parse `target/catalog.json` (emitted by `dbt docs
	 * generate`). Returns `null` if no catalog has been generated yet.
	 */
	async readCatalog(projectDir: string): Promise<DbtCatalog | null> {
		const raw = await this._readTargetJson(projectDir, "catalog.json");
		if (!raw || typeof raw !== "object") return null;
		const r = raw as Record<string, unknown>;
		return {
			nodes: (r.nodes as Record<string, DbtCatalogEntry>) ?? {},
			sources: (r.sources as Record<string, DbtCatalogEntry>) ?? undefined,
			errors: (r.errors as Record<string, string>) ?? undefined,
		};
	}

	private async _readTargetJson(
		projectDir: string,
		filename: string,
	): Promise<unknown | null> {
		const cleanedDir = projectDir.replace(/\/+$/, "");
		const path = `${cleanedDir}/target/${filename}`;
		try {
			const bytes = await this.aos.readFile(path);
			return JSON.parse(new TextDecoder().decode(bytes));
		} catch {
			return null;
		}
	}

	/**
	 * Read the current dbt bootstrap tripwire counters directly from the
	 * Pyodide worker. Returns null if the VM wasn't created with
	 * `python.dbt: true` (the tripwire module won't be loaded).
	 *
	 * Useful for passive observation outside of a `run` call — e.g. the
	 * playground polls this to animate counter increments as agent code
	 * runs.
	 */
	async tripwire(): Promise<DbtTripwireSnapshot | null> {
		let out = "";
		const decoder = new TextDecoder();
		const { pid } = this.aos.spawn(
			"python3",
			["-c", DBT_TRIPWIRE_PROBE_PY],
			{
				onStdout: (chunk) => {
					out += decoder.decode(chunk, { stream: true });
				},
			},
		);
		await this.aos.waitProcess(pid);
		out += decoder.decode();
		return parseDbtTripwireProbe(out);
	}
}
