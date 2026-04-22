/**
 * Validate that the dbt bootstrap script is well-formed Python that
 * actually executes without errors.
 *
 * We run it through the host's CPython rather than Pyodide so this test
 * runs in vanilla CI without needing the Pyodide runtime. The bootstrap
 * code only uses stdlib (multiprocessing, threading, types, os) so the
 * monkey-patch logic is testable on any CPython >= 3.9.
 */
import { execSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { DBT_BOOTSTRAP_SCRIPT } from "../../src/dbt-bootstrap.ts";

function runPython(source: string): { code: number; stdout: string; stderr: string } {
	const dir = mkdtempSync(join(tmpdir(), "dbt-bootstrap-test-"));
	const file = join(dir, "harness.py");
	writeFileSync(file, source);
	try {
		const stdout = execSync(`python3 ${file}`, {
			encoding: "utf-8",
			stdio: ["ignore", "pipe", "pipe"],
		});
		return { code: 0, stdout, stderr: "" };
	} catch (err) {
		const e = err as {
			status?: number;
			stdout?: Buffer | string;
			stderr?: Buffer | string;
		};
		return {
			code: e.status ?? -1,
			stdout: typeof e.stdout === "string" ? e.stdout : (e.stdout?.toString() ?? ""),
			stderr: typeof e.stderr === "string" ? e.stderr : (e.stderr?.toString() ?? ""),
		};
	}
}

function pythonAvailable(): boolean {
	try {
		execSync("python3 --version", { stdio: "ignore" });
		return true;
	} catch {
		return false;
	}
}

const HAS_PYTHON = pythonAvailable();

describe.skipIf(!HAS_PYTHON)("dbt bootstrap script — real-CPython validation", () => {
	it("multiprocessing.get_context('spawn') returns a working stub", () => {
		const harness = `${DBT_BOOTSTRAP_SCRIPT}
import multiprocessing
ctx = multiprocessing.get_context('spawn')
for name in ('Lock', 'RLock', 'Semaphore', 'BoundedSemaphore', 'Event', 'Condition', 'Queue'):
    assert hasattr(ctx, name), f'{name} missing from stub'

# Lock acquire/release works
lock = ctx.Lock()
lock.acquire()
lock.release()

# RLock is reentrant
rlock = ctx.RLock()
rlock.acquire(); rlock.acquire(); rlock.release(); rlock.release()

# Queue is functional
q = ctx.Queue()
q.put('a'); q.put('b')
assert q.get() == 'a'
assert q.get() == 'b'

# Manager intentionally raises
try:
    ctx.Manager()
    raise AssertionError('Manager() should have raised')
except NotImplementedError:
    pass

print('STUB_OK')
`;
		const r = runPython(harness);
		expect(r.stderr, r.stderr).toBe("");
		expect(r.code).toBe(0);
		expect(r.stdout).toContain("STUB_OK");
	});

	it("is idempotent — re-running does not re-patch get_context", () => {
		const escaped = JSON.stringify(DBT_BOOTSTRAP_SCRIPT);
		const harness = `${DBT_BOOTSTRAP_SCRIPT}
import multiprocessing
first_patcher = multiprocessing.get_context
assert multiprocessing._agent_os_dbt_patched is True

# Re-execute the bootstrap. The sentinel marker should prevent re-patching.
exec(${escaped})
assert multiprocessing.get_context is first_patcher, \\
    'Bootstrap is not idempotent: get_context was replaced on second run'
print('IDEMPOTENT_OK')
`;
		const r = runPython(harness);
		expect(r.stderr, r.stderr).toBe("");
		expect(r.code).toBe(0);
		expect(r.stdout).toContain("IDEMPOTENT_OK");
	});

	it("sets the required dbt environment variables", () => {
		const harness = `${DBT_BOOTSTRAP_SCRIPT}
import os
required = {
    'DBT_SINGLE_THREADED': 'True',
    'DBT_SEND_ANONYMOUS_USAGE_STATS': 'False',
    'DBT_STATIC_PARSER': 'False',
    'DBT_USE_EXPERIMENTAL_PARSER': 'False',
    'DBT_PARTIAL_PARSE': 'False',
    'DBT_VERSION_CHECK': 'False',
    'PROTOCOL_BUFFERS_PYTHON_IMPLEMENTATION': 'python',
}
for k, v in required.items():
    assert os.environ.get(k) == v, f'{k} expected {v!r}, got {os.environ.get(k)!r}'
print('ENV_OK')
`;
		const r = runPython(harness);
		expect(r.stderr, r.stderr).toBe("");
		expect(r.code).toBe(0);
		expect(r.stdout).toContain("ENV_OK");
	});

	it("does not override pre-set env vars (uses setdefault)", () => {
		const harness = `import os
os.environ['DBT_SINGLE_THREADED'] = 'CUSTOM'
${DBT_BOOTSTRAP_SCRIPT}
assert os.environ['DBT_SINGLE_THREADED'] == 'CUSTOM', 'setdefault was bypassed'
print('SETDEFAULT_OK')
`;
		const r = runPython(harness);
		expect(r.stderr, r.stderr).toBe("");
		expect(r.code).toBe(0);
		expect(r.stdout).toContain("SETDEFAULT_OK");
	});

	it("falls back to the original get_context for non-spawn methods", () => {
		const harness = `${DBT_BOOTSTRAP_SCRIPT}
import multiprocessing
# 'fork' should fall through to the real get_context (which on macOS may
# return a ForkContext or raise — either way, our stub should NOT be returned).
import types
try:
    ctx = multiprocessing.get_context('fork')
    assert not isinstance(ctx, types.SimpleNamespace), 'fork returned the stub'
except (ValueError, NotImplementedError):
    # Some platforms reject 'fork'; that's the original behavior, also fine.
    pass
print('PASSTHROUGH_OK')
`;
		const r = runPython(harness);
		expect(r.stderr, r.stderr).toBe("");
		expect(r.code).toBe(0);
		expect(r.stdout).toContain("PASSTHROUGH_OK");
	});

	it("exposes tripwire counters incremented by the patched shims", () => {
		// The tripwire module is the observability surface callers use to
		// prove the monkey-patches actually fired during a dbt run. This
		// test hits each shim once and confirms the counters move.
		const harness = `${DBT_BOOTSTRAP_SCRIPT}
import sys
trip = sys.modules['_agent_os_dbt_tripwire']

# Baseline counters
base_submit = trip.thread_pool_executor_submit
base_ctx = trip.multiprocessing_get_context

# Trip the sync ThreadPoolExecutor.submit shim
from concurrent.futures import ThreadPoolExecutor
with ThreadPoolExecutor() as exe:
    fut = exe.submit(lambda: 42)
    assert fut.result() == 42

# Trip the get_context shim
import multiprocessing
multiprocessing.get_context('spawn')

assert trip.thread_pool_executor_submit == base_submit + 1, \\
    f'submit counter did not move: {trip.thread_pool_executor_submit}'
assert trip.multiprocessing_get_context == base_ctx + 1, \\
    f'get_context counter did not move: {trip.multiprocessing_get_context}'
print('TRIPWIRE_OK')
`;
		const r = runPython(harness);
		expect(r.stderr, r.stderr).toBe("");
		expect(r.code).toBe(0);
		expect(r.stdout).toContain("TRIPWIRE_OK");
	});
});
